//! Runner for collections and stress batteries with per-endpoint statistics.

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    time::{Duration, Instant},
};

use reqwest::blocking::Client;
use serde::Serialize;

use crate::{
    domain::{
        model::{self, Collection, MAX_VUS, Request, Response, Stage},
        vars::{Vars, parse_headers, resolve},
    },
    engine::script::{self, Outcome, ReqParts, Scopes, TestResult},
    i18n::t,
    infra::http,
    tr,
};

pub struct SendDone {
    pub scopes: Scopes,
    /// The request as it ended up after the pre-request (variables unresolved).
    pub req: ReqParts,
    pub result: Result<Response, String>,
    pub tests: Vec<TestResult>,
    pub logs: Vec<String>,
    /// Last pm.setNextRequest (pre-request or tests).
    pub next: Option<Option<String>>,
}

fn collect(o: Outcome, stage: &str, d: &mut SendDone) {
    d.tests.extend(o.tests);
    d.logs.extend(o.logs);
    if o.next.is_some() {
        d.next = o.next;
    }
    if let Some(e) = o.error {
        d.tests.push(TestResult { name: stage.into(), pass: false, error: Some(e) });
    }
}

/// pre-request -> resolve variables -> HTTP -> tests.
pub fn run_send(client: &Client, ed: &Request, mut scopes: Scopes) -> SendDone {
    let mut req = ed.parts();
    let pre = script::run(&ed.pre, &mut scopes, &mut req, None);
    let vars = scopes.merged();
    // Auth is applied after the pre-request (which may have set the token) and goes into `req`,
    // so stress testing reuses the already authorized request.
    let result = crate::infra::auth::apply(client, &ed.auth, &mut req, &vars).and_then(|_| send_parts(client, &req, &vars));
    let mut d = SendDone { scopes, req, result, tests: Vec::new(), logs: Vec::new(), next: None };
    collect(pre, "Pre-request", &mut d);
    if let Ok(r) = &d.result {
        let o = script::run(&ed.tests, &mut d.scopes, &mut d.req, Some(r));
        collect(o, "Tests", &mut d);
    }
    d
}

fn send_parts(client: &Client, req: &ReqParts, vars: &Vars) -> Result<Response, String> {
    let r = |s: &str| resolve(s, vars);
    http::send_http(client, &req.method, &r(&req.url), &parse_headers(&r(&req.headers)), &r(&req.body))
}

// ---- Data files ----

/// CSV (with header; "," or ";" separator, double quotes) or JSON (array of objects) -> one row per iteration.
pub fn parse_data(text: &str) -> Result<Vec<Vars>, String> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim_start().starts_with('[') {
        let rows: Vec<serde_json::Map<String, serde_json::Value>> = serde_json::from_str(text).map_err(|e| tr!("JSON de datos inválido: {e}", "invalid data JSON: {e}"))?;
        return Ok(rows
            .into_iter()
            .map(|r| r.into_iter().map(|(k, v)| (k, v.as_str().map_or_else(|| v.to_string(), String::from))).collect())
            .collect());
    }
    let first = text.lines().next().unwrap_or("");
    let sep = if first.contains(';') && !first.contains(',') { ';' } else { ',' };
    let mut rows = csv_rows(text, sep).into_iter();
    let header = rows.next().ok_or(t("el archivo de datos está vacío", "the data file is empty"))?;
    let header: Vec<String> = header.into_iter().map(|h| h.trim().to_string()).collect();
    Ok(rows
        .filter(|r| r.iter().any(|c| !c.is_empty()))
        .map(|r| header.iter().cloned().zip(r.into_iter().chain(std::iter::repeat(String::new()))).collect())
        .collect())
}

fn csv_rows(text: &str, sep: char) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut cell, mut quoted) = (Vec::new(), Vec::new(), String::new(), false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => row.push(std::mem::take(&mut cell)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            c => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

// ---- Runner ----

#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub iteration: u32,
    pub name: String,
    pub method: String,
    /// 0 if there was no response.
    pub status: u16,
    pub ms: u64,
    pub tests: Vec<TestResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Step {
    pub fn passed(&self) -> bool {
        self.error.is_none() && self.tests.iter().all(|t| t.pass)
    }
}

// ponytail: fixed cap of steps per iteration to cut setNextRequest loops; raise it if legitimate polling hits it.
const MAX_STEPS_PER_ITERATION: usize = 10_000;

/// Runs the requests in order, `iterations` times. With `data`, each iteration uses one row (pm.iterationData
/// and {{column}}), wrapping around if there are more iterations than rows. Variables set by the
/// scripts carry over to the next request; locals are cleared for each one.
/// pm.setNextRequest(name) jumps to that request and pm.setNextRequest(null) ends the iteration.
#[allow(clippy::too_many_arguments)]
pub fn run_collection(
    client: &Client,
    col: &Collection,
    scopes: &mut Scopes,
    iterations: u32,
    data: &[Vars],
    bail: bool,
    stop: &AtomicBool,
    on_step: &mut dyn FnMut(&Step),
) -> Vec<Step> {
    let mut steps = Vec::new();
    for iteration in 1..=iterations.max(1) {
        scopes.data = if data.is_empty() { Vars::new() } else { data[(iteration as usize - 1) % data.len()].clone() };
        let (mut i, mut count) = (0, 0);
        while let Some(r) = col.requests.get(i) {
            if stop.load(Relaxed) {
                return steps;
            }
            scopes.local.clear();
            let d = run_send(client, &model::with_effective_auth(r, Some(col)), std::mem::take(scopes));
            *scopes = d.scopes;
            let (status, ms) = d.result.as_ref().map_or((0, 0), |r| (r.status, r.ms));
            let mut step = Step { iteration, name: r.name.clone(), method: r.method.clone(), status, ms, tests: d.tests, error: d.result.err() };
            count += 1;
            i = match d.next {
                None => i + 1,
                Some(None) => usize::MAX,
                Some(Some(name)) => col.requests.iter().position(|q| q.name == name).unwrap_or_else(|| {
                    step.tests.push(TestResult { name: "setNextRequest".into(), pass: false, error: Some(tr!("no existe el request \"{name}\"", "the request \"{name}\" does not exist")) });
                    usize::MAX
                }),
            };
            if count >= MAX_STEPS_PER_ITERATION && i < col.requests.len() {
                step.tests.push(TestResult {
                    name: "setNextRequest".into(),
                    pass: false,
                    error: Some(tr!("más de {MAX_STEPS_PER_ITERATION} pasos en la iteración: ¿bucle infinito?", "more than {MAX_STEPS_PER_ITERATION} steps in the iteration: infinite loop?")),
                });
                i = usize::MAX;
            }
            on_step(&step);
            let failed = !step.passed();
            steps.push(step);
            if bail && failed {
                return steps;
            }
        }
    }
    steps
}

/// JUnit report: Azure DevOps shows it with the "Publish Test Results" task.
pub fn junit(suite: &str, steps: &[Step]) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let failures = steps.iter().filter(|s| !s.passed()).count();
    let mut x = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"{}\" tests=\"{}\" failures=\"{failures}\">\n", esc(suite), steps.len());
    for s in steps {
        let fails = s.tests.iter().filter(|t| !t.pass).count() + usize::from(s.error.is_some());
        let cases = s.tests.len() + usize::from(s.error.is_some() || s.tests.is_empty());
        x += &format!(
            "  <testsuite name=\"{} {} #{}\" tests=\"{cases}\" failures=\"{fails}\" time=\"{:.3}\">\n",
            esc(&s.method),
            esc(&s.name),
            s.iteration,
            s.ms as f64 / 1000.0
        );
        if let Some(e) = &s.error {
            x += &format!("    <testcase name=\"request\"><failure message=\"{}\"/></testcase>\n", esc(e));
        } else if s.tests.is_empty() {
            x += &format!("    <testcase name=\"HTTP {}\"/>\n", s.status);
        }
        for t in &s.tests {
            match &t.error {
                Some(e) if !t.pass => x += &format!("    <testcase name=\"{}\"><failure message=\"{}\"/></testcase>\n", esc(&t.name), esc(e)),
                _ if !t.pass => x += &format!("    <testcase name=\"{}\"><failure/></testcase>\n", esc(&t.name)),
                _ => x += &format!("    <testcase name=\"{}\"/>\n", esc(&t.name)),
            }
        }
        x += "  </testsuite>\n";
    }
    x + "</testsuites>\n"
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub name: String,
    pub count: usize,
    /// 2xx/3xx responses.
    pub ok: usize,
    pub error_pct: f64,
    pub rps: f64,
    pub min: u64,
    pub mean: u64,
    pub p50: u64,
    pub p90: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
    pub bytes: usize,
    /// "200" -> 950, "503" -> 12, "error" -> 3
    pub statuses: BTreeMap<String, usize>,
    /// First distinct network errors.
    pub errors: Vec<String>,
}

/// One second of the stage, for the latency-over-time charts.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Bucket {
    pub sec: u64,
    pub vus: u32,
    pub count: usize,
    pub errors: usize,
    pub p50: u64,
    pub p95: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StageReport {
    pub vus: u32,
    pub secs: u64,
    pub ramp: bool,
    pub elapsed_ms: u64,
    pub total: Stats,
    pub endpoints: Vec<Stats>,
    pub timeline: Vec<Bucket>,
}

pub struct Progress {
    pub stage: usize,
    pub stages: usize,
    pub vus: u32,
    pub elapsed: Duration,
    pub secs: u64,
    pub count: usize,
    pub failed: usize,
}

struct Sample {
    req: usize,
    /// End of the request, in ms since the stage started.
    at: u64,
    ms: u64,
    status: Option<u16>,
    bytes: usize,
    err: Option<String>,
}

impl Sample {
    fn ok(&self) -> bool {
        self.status.is_some_and(|c| (200..400).contains(&c))
    }
}

/// 1) `setup` (e.g. login) is sent once with its scripts; if it fails, nothing is measured.
/// 2) Warm-up: each endpoint once in order with pre-request and tests (not counted in the stats).
/// 3) Hammers the prepared requests stage by stage.
/// ponytail: does not run tests per request (Boa per request would measure the script, not the API); success is 2xx/3xx.
pub fn stress(
    client: &Client,
    setup: &[Request],
    targets: &[Request],
    mut scopes: Scopes,
    stages: &[Stage],
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(&Progress),
) -> Result<Vec<StageReport>, String> {
    if targets.is_empty() || stages.is_empty() {
        return Err(t("No hay endpoints o etapas que ejecutar", "There are no endpoints or stages to run").into());
    }
    for s in setup {
        scopes.local.clear();
        let d = run_send(client, s, scopes);
        let problem = match &d.result {
            Err(e) => Some(e.clone()),
            Ok(r) if r.status >= 400 => Some(format!("HTTP {}", r.status)),
            Ok(_) => d.tests.iter().find(|t| !t.pass).map(|t| format!("{}: {}", t.name, t.error.clone().unwrap_or_default())),
        };
        if let Some(p) = problem {
            return Err(tr!("La preparación \"{}\" falló ({p}); no se ejecutó la batería", "The setup \"{}\" failed ({p}); the battery did not run", s.name));
        }
        scopes = d.scopes;
    }
    let mut warm = Vec::new();
    for t in targets {
        if stop.load(Relaxed) {
            return Ok(Vec::new());
        }
        scopes.local.clear();
        let d = run_send(client, t, scopes);
        if let Some(e) = d.tests.iter().find(|x| x.name == "Pre-request").and_then(|x| x.error.as_ref()) {
            return Err(tr!("Pre-request de \"{}\": {e}", "Pre-request of \"{}\": {e}", t.name));
        }
        scopes = d.scopes;
        warm.push((d.req, scopes.local.clone()));
    }
    scopes.local.clear();
    let prepared: Vec<(ReqParts, Vars)> = warm
        .into_iter()
        .map(|(parts, local)| {
            let mut v = scopes.merged();
            v.extend(local);
            (parts, v)
        })
        .collect();

    let mut reports = Vec::new();
    let mut prev_vus = 1;
    for (si, st) in stages.iter().enumerate() {
        if stop.load(Relaxed) {
            break;
        }
        let st = Stage { vus: st.vus.clamp(1, MAX_VUS), ..st.clone() };
        let from = prev_vus;
        let workers_n = if st.ramp { st.vus.max(from) } else { st.vus };
        let (count, failed) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let start = Instant::now();
        let deadline = start + Duration::from_secs(st.secs.max(1));
        let samples: Vec<Sample> = std::thread::scope(|sc| {
            let workers: Vec<_> = (0..workers_n)
                .map(|vu| {
                    let (prepared, count, failed, st) = (&prepared, &count, &failed, &st);
                    sc.spawn(move || {
                        let mut out = Vec::new();
                        // Each user starts on a different endpoint to spread the load.
                        let mut i = vu as usize % prepared.len();
                        while Instant::now() < deadline && !stop.load(Relaxed) {
                            // In a ramp, user `vu` waits until it is its turn to be active.
                            if vu >= st.vus_at(from, start.elapsed().as_secs_f64()) {
                                std::thread::sleep(Duration::from_millis(20));
                                continue;
                            }
                            let (parts, vars) = &prepared[i];
                            let t = Instant::now();
                            let res = send_parts(client, parts, vars);
                            let at = start.elapsed().as_millis() as u64;
                            let s = match res {
                                Ok(r) => Sample { req: i, at, ms: r.ms, status: Some(r.status), bytes: r.size, err: None },
                                Err(e) => Sample { req: i, at, ms: t.elapsed().as_millis() as u64, status: None, bytes: 0, err: Some(e) },
                            };
                            count.fetch_add(1, Relaxed);
                            if !s.ok() {
                                failed.fetch_add(1, Relaxed);
                            }
                            out.push(s);
                            i = (i + 1) % prepared.len();
                        }
                        out
                    })
                })
                .collect();
            while Instant::now() < deadline && !stop.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                let elapsed = start.elapsed();
                on_progress(&Progress {
                    stage: si,
                    stages: stages.len(),
                    vus: st.vus_at(from, elapsed.as_secs_f64()),
                    elapsed,
                    secs: st.secs,
                    count: count.load(Relaxed),
                    failed: failed.load(Relaxed),
                });
            }
            workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
        });
        let elapsed = start.elapsed();
        let all: Vec<&Sample> = samples.iter().collect();
        reports.push(StageReport {
            vus: st.vus,
            secs: st.secs,
            ramp: st.ramp,
            elapsed_ms: elapsed.as_millis() as u64,
            total: stats("Total", &all, elapsed),
            endpoints: targets
                .iter()
                .enumerate()
                .map(|(i, t)| stats(&format!("{} {}", t.method, t.name), &samples.iter().filter(|s| s.req == i).collect::<Vec<_>>(), elapsed))
                .collect(),
            timeline: timeline(&samples, &st, from, elapsed),
        });
        prev_vus = st.vus;
    }
    Ok(reports)
}

fn percentile(sorted: &[u64], q: f64) -> u64 {
    let n = sorted.len();
    // Nearest rank.
    if n == 0 { 0 } else { sorted[((q / 100.0 * n as f64).ceil() as usize).clamp(1, n) - 1] }
}

fn stats(name: &str, samples: &[&Sample], elapsed: Duration) -> Stats {
    let mut lat: Vec<u64> = samples.iter().map(|s| s.ms).collect();
    lat.sort_unstable();
    let n = lat.len();
    let ok = samples.iter().filter(|s| s.ok()).count();
    let mut statuses = BTreeMap::new();
    let mut errors: Vec<String> = Vec::new();
    for s in samples {
        *statuses.entry(s.status.map_or("error".into(), |c| c.to_string())).or_insert(0) += 1;
        if let Some(e) = &s.err {
            if errors.len() < 5 && !errors.contains(e) {
                errors.push(e.clone());
            }
        }
    }
    Stats {
        name: name.into(),
        count: n,
        ok,
        error_pct: if n == 0 { 0.0 } else { (n - ok) as f64 * 100.0 / n as f64 },
        rps: n as f64 / elapsed.as_secs_f64().max(0.001),
        min: lat.first().copied().unwrap_or(0),
        mean: if n == 0 { 0 } else { lat.iter().sum::<u64>() / n as u64 },
        p50: percentile(&lat, 50.0),
        p90: percentile(&lat, 90.0),
        p95: percentile(&lat, 95.0),
        p99: percentile(&lat, 99.0),
        max: lat.last().copied().unwrap_or(0),
        bytes: samples.iter().map(|s| s.bytes).sum(),
        statuses,
        errors,
    }
}

fn timeline(samples: &[Sample], st: &Stage, from: u32, elapsed: Duration) -> Vec<Bucket> {
    // One bucket per planned second; whatever finishes after the close (in-flight requests) lands in the last one.
    let secs = (elapsed.as_secs_f64().ceil() as usize).clamp(1, st.secs.max(1) as usize);
    let mut lat: Vec<Vec<u64>> = vec![Vec::new(); secs];
    let mut errors = vec![0; secs];
    for s in samples {
        let b = ((s.at / 1000) as usize).min(secs - 1);
        lat[b].push(s.ms);
        errors[b] += usize::from(!s.ok());
    }
    lat.into_iter()
        .zip(errors)
        .enumerate()
        .map(|(sec, (mut l, errors))| {
            l.sort_unstable();
            Bucket { sec: sec as u64, vus: st.vus_at(from, sec as f64 + 0.5), count: l.len(), errors, p50: percentile(&l, 50.0), p95: percentile(&l, 95.0) }
        })
        .collect()
}

/// CI thresholds: returns the violations (empty = pass).
pub fn check_thresholds(reports: &[StageReport], max_p95: Option<u64>, max_error_pct: Option<f64>) -> Vec<String> {
    let mut v = Vec::new();
    for (i, r) in reports.iter().enumerate() {
        for s in std::iter::once(&r.total).chain(&r.endpoints).filter(|s| s.count > 0) {
            if let Some(m) = max_p95.filter(|&m| s.p95 > m) {
                v.push(tr!("Etapa {} ({} VUs) · {}: p95 {} ms > {m} ms", "Stage {} ({} VUs) · {}: p95 {} ms > {m} ms", i + 1, r.vus, s.name, s.p95));
            }
            if let Some(m) = max_error_pct.filter(|&m| s.error_pct > m) {
                v.push(tr!("Etapa {} ({} VUs) · {}: errores {:.2}% > {m}%", "Stage {} ({} VUs) · {}: errors {:.2}% > {m}%", i + 1, r.vus, s.name, s.error_pct));
            }
        }
    }
    v
}

/// "▁▂▅█" for the CLI.
pub fn sparkline(values: &[f64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().cloned().fold(0.0, f64::max);
    values.iter().map(|v| if max <= 0.0 { BARS[0] } else { BARS[((v / max) * 7.0).round() as usize] }).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    use super::*;

    /// Minimal HTTP server: replies 200 with {"path": ...}, or 500 on /fail.
    fn server() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&buf);
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let (code, body) = if path == "/fail" { ("500 Internal Server Error", "{}".into()) } else { ("200 OK", format!("{{\"path\":\"{path}\"}}")) };
                    let _ = write!(s, "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                });
            }
        });
        format!("http://{addr}")
    }

    fn req(name: &str, url: &str, tests: &str) -> Request {
        Request { name: name.into(), method: "GET".into(), url: url.into(), tests: tests.into(), ..Default::default() }
    }

    #[test]
    fn runner_chains_variables_and_reports() {
        let base = server();
        let col = Collection {
            name: "c".into(),
            requests: vec![
                req("login", "{{base}}/token123", "pm.environment.set('tok', pm.response.json().path.slice(1)); pm.test('200', () => pm.response.to.have.status(200));"),
                req("uses", "{{base}}/x/{{tok}}", "pm.test('chains', () => pm.expect(pm.response.json().path).to.eql('/x/token123'));"),
                req("fails", "{{base}}/fail", "pm.test('ok', () => pm.response.to.have.status(200));"),
                req("never", "{{base}}/y", ""),
            ],
            ..Default::default()
        };
        let mut scopes = Scopes { env: [("base".into(), base)].into(), ..Default::default() };
        let stop = AtomicBool::new(false);
        let mut seen = 0;
        let steps = run_collection(&http::client(), &col, &mut scopes, 1, &[], true, &stop, &mut |_| seen += 1);
        assert_eq!((steps.len(), seen), (3, 3), "bail stops at the first failure");
        assert!(steps[0].passed() && steps[1].passed() && !steps[2].passed(), "{steps:?}");
        assert_eq!(scopes.env["tok"], "token123");
        let x = junit("c <&>", &steps);
        assert!(x.contains("name=\"c &lt;&amp;&gt;\"") && x.contains("failures=\"1\"") && x.contains("<failure"), "{x}");
    }

    #[test]
    fn runner_data_rows_and_set_next_request() {
        let base = server();
        // "a" uses the data row; "polling" repeats up to 3 times with setNextRequest; "skipped" never runs.
        let col = Collection {
            name: "c".into(),
            requests: vec![
                req("a", "{{base}}/u/{{role}}", "pm.test('datum', () => pm.expect(pm.response.json().path).to.eql('/u/' + pm.iterationData.get('role')));"),
                req(
                    "polling",
                    "{{base}}/p",
                    "const n = Number(pm.environment.get('n') || 0) + 1; pm.environment.set('n', n); pm.setNextRequest(n < 3 ? 'polling' : 'end');",
                ),
                req("skipped", "{{base}}/never", ""),
                req("end", "{{base}}/end", "pm.environment.unset('n'); pm.setNextRequest(null);"),
                req("after-null", "{{base}}/never", ""),
            ],
            ..Default::default()
        };
        let data = parse_data("user;role\n\"ana; \"\"the boss\"\"\";admin\r\nbeto;dev\n").unwrap();
        assert_eq!(data[0]["user"], "ana; \"the boss\"");
        let mut scopes = Scopes { env: [("base".into(), base)].into(), ..Default::default() };
        let stop = AtomicBool::new(false);
        let steps = run_collection(&http::client(), &col, &mut scopes, 3, &data, false, &stop, &mut |_| {});
        let names: Vec<&str> = steps.iter().filter(|s| s.iteration == 1).map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a", "polling", "polling", "polling", "end"]);
        assert_eq!(steps.len(), 15);
        assert!(steps.iter().all(|s| s.passed()), "{steps:?}");

        let bad = Collection { requests: vec![req("x", "{{base}}/x", "pm.setNextRequest('missing')")], ..Default::default() };
        let steps = run_collection(&http::client(), &bad, &mut scopes, 1, &[], false, &stop, &mut |_| {});
        assert!(!steps[0].passed());
        assert_eq!(parse_data(r#"[{"id": 7, "n": "x"}]"#).unwrap()[0]["id"], "7");
    }

    #[test]
    fn stress_collects_stats_per_endpoint() {
        let base = server();
        let targets = [req("ok", &format!("{base}/a/{{{{$guid}}}}"), ""), req("err", &format!("{base}/fail"), "")];
        let stop = AtomicBool::new(false);
        let mut ticks = 0;
        let stages = [Stage { vus: 4, secs: 1, ramp: false }, Stage { vus: 8, secs: 2, ramp: true }];
        let r = stress(&http::client(), &[], &targets, Scopes::default(), &stages, &stop, &mut |_| ticks += 1).unwrap();
        let (t, ok, err) = (&r[0].total, &r[0].endpoints[0], &r[0].endpoints[1]);
        assert!(ticks > 0 && t.count > 10 && t.count == ok.count + err.count, "{t:?}");
        assert_eq!((ok.ok, err.ok, err.error_pct), (ok.count, 0, 100.0));
        assert_eq!(err.statuses["500"], err.count);
        assert!(t.min <= t.p50 && t.p50 <= t.p95 && t.p95 <= t.p99 && t.p99 <= t.max);
        assert!(!check_thresholds(&r, None, Some(1.0)).is_empty() && check_thresholds(&r, None, None).is_empty());
        // The 4→8 ramp shows in the timeline and each second adds up what was measured.
        let tl = &r[1].timeline;
        assert!(tl.len() >= 2 && tl[0].vus < tl[tl.len() - 1].vus && tl[tl.len() - 1].vus <= 8, "{tl:?}");
        assert_eq!((tl.len(), tl.iter().map(|b| b.count).sum::<usize>()), (2, r[1].total.count));

        let login = req("login", &format!("{base}/fail"), "");
        let e = stress(&http::client(), &[login], &targets, Scopes::default(), &stages, &stop, &mut |_| {}).unwrap_err();
        assert!(e.contains("login") && e.contains("500"), "{e}");
    }

    #[test]
    fn percentiles_ramps_and_sparkline() {
        let s: Vec<Sample> = (1..=100).map(|ms| Sample { req: 0, at: 0, ms, status: Some(200), bytes: 1, err: None }).collect();
        let st = stats("x", &s.iter().collect::<Vec<_>>(), Duration::from_secs(2));
        assert_eq!((st.p50, st.p90, st.p95, st.p99, st.max, st.mean, st.rps), (50, 90, 95, 99, 100, 50, 50.0));
        let ramp = Stage { vus: 100, secs: 10, ramp: true };
        assert_eq!((ramp.vus_at(1, 0.0), ramp.vus_at(1, 5.0), ramp.vus_at(1, 10.0), ramp.vus_at(1, 99.0)), (1, 51, 100, 100));
        assert_eq!(Stage { vus: 1, secs: 10, ramp: true }.vus_at(50, 10.0), 1);
        assert_eq!(sparkline(&[0.0, 5.0, 10.0]), "▁▅█");
    }
}
