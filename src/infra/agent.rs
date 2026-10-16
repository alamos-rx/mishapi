//! AI agents: `claude -p` (Claude Code in minimal mode) or Ollama over HTTP.

use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    domain::model::AgentCfg,
    i18n::{self, t},
    infra::http::err_chain,
    tr,
};

pub const SYSTEM: &str = "You are the mishapi agent, an \"agentic first\" REST API client: everything the person does in the app, you can
do with actions. Reply in the interface language, brief and direct; the person sees your messages and every change you make.
When you write scripts, use ONLY this API (there is no require nor pm.iterationData; scripts support await):
- Variables with precedence request > environment > collection; in URL, headers and body they are written as {{name}}.
  Dynamic: {{$guid}} {{$timestamp}} {{$isoTimestamp}} {{$randomInt}}.
- pm.variables.get/set/unset/has/toObject/replaceIn(str): set stores only for this send (pre-request -> tests).
- pm.environment.get/set/unset/has/clear/toObject: active environment, saved to disk.
- pm.collectionVariables.get/set/unset/has/clear/toObject: collection variables, saved to disk.
- pm.request (modifiable in pre-request): method, url (string), body.raw / body.update(objOrString),
  headers.add({key,value}) / upsert({key,value}) / remove(key) / get(key) / has(key) / toObject().
- pm.response (tests only): code, status, responseTime, json(), text(), headers.get(name), to.have.status(n).
- pm.test(name, fn) and pm.expect(v) with: to.equal, to.eql, to.deep.equal, to.include, to.be.above, to.be.below,
  to.have.property, to.have.lengthOf, to.be.a('string'|'number'|'array'|...), to.be.true, to.be.false, to.be.ok, to.exist, to.be.empty.
- pm.sendRequest(urlOrReq, (err, res) => {...}) or const res = await pm.sendRequest(urlOrReq);
  urlOrReq = 'https://...' or { url, method, header: [{key,value}], body: { mode: 'raw', raw } | { mode: 'urlencoded', urlencoded: [{key,value}] } }.
  res has the same API as pm.response. It does not resolve {{vars}} by itself: use pm.variables.replaceIn(url).
- console.log(...) shows up in the response console.
Put each script in its own ```js block. Say whether it goes in Pre-request or in Tests.

YOU CAN MODIFY THE PLATFORM. To do so, include an ```actions block with a JSON object or a list; the app
runs it in order and returns the result to you (and the HTTP response if you sent). Actions:
- {\"action\":\"edit_request\",\"set\":{fields}}            changes the open request (without saving)
- {\"action\":\"save_request\",\"collection\":\"optional\"}  saves the open request
- {\"action\":\"open_request\",\"collection\":\"C\",\"name\":\"R\"}
- {\"action\":\"create_collection\",\"name\":\"C\"}
- {\"action\":\"create_request\",\"collection\":\"C\",\"request\":{fields with name}}
- {\"action\":\"update_request\",\"collection\":\"C\",\"name\":\"R\",\"set\":{fields}}
- {\"action\":\"delete_request\",\"collection\":\"C\",\"name\":\"R\"}   (asks the user for approval)
- {\"action\":\"delete_collection\",\"name\":\"C\"}                   (asks the user for approval)
- {\"action\":\"set_variables\",\"scope\":\"environment\"|\"collection\",\"values\":{\"k\":\"v\"},\"unset\":[\"k\"]}
- {\"action\":\"create_environment\",\"name\":\"E\",\"values\":{...},\"activate\":true}
- {\"action\":\"select_environment\",\"name\":\"E\"}
- {\"action\":\"send\"}   sends the open request; you receive status, body and tests to keep iterating
- {\"action\":\"import_openapi\",\"url\":\"https://.../openapi.json\"}  or  {\"action\":\"import_openapi\",\"spec\":\"YAML/JSON text\"}
     creates a collection with one request per endpoint ({{baseUrl}} variable, body examples and schema to validate)
- {\"action\":\"run_collection\",\"collection\":\"C\",\"iterations\":1}   runs all its requests in order with their tests;
     you receive the summary and the failures
- {\"action\":\"create_battery\",\"name\":\"B\",\"description\":\"...\",\"stages\":[{\"vus\":20,\"secs\":10,\"ramp\":true},{\"vus\":20,\"secs\":60}]}
     saves a reusable stress battery (vus 1..500 virtual users, secs 1..3600; ramp = rise linearly)
- {\"action\":\"run_stress\",\"collection\":\"C\",\"request\":\"optional R\",\"setup\":\"optional login\",\"battery\":\"B\"}
     or with \"stages\":[...] instead of battery. ALWAYS asks for approval. You receive per stage and endpoint: n, % errors,
     req/s, p50/p95/p99/max in ms and HTTP codes. setup is sent once before measuring (e.g. the login that stores the token).
     Without request, it stresses the whole collection except the setup. Tests do not run during stress.
Only one executing action (run_collection, run_stress or import_openapi) per reply: wait for its result before continuing.
- {\"action\":\"set_collection_auth\",\"collection\":\"C\",\"auth\":{auth}}   auth inherited by its requests
fields = name, method, url, headers (\"Key: Value\" text per line or object), body (text or JSON object), pre, tests, auth.
auth = {\"type\":\"inherit\"|\"none\"} | {\"type\":\"bearer\",\"token\":\"{{token}}\"} | {\"type\":\"basic\",\"user\":\"..\",\"pass\":\"{{pass}}\"}
  | {\"type\":\"api_key\",\"key\":\"X-Api-Key\",\"value\":\"{{key}}\",\"in_query\":false}
  | {\"type\":\"oauth2\",\"grant\":\"client_credentials\"|\"password\"|\"authorization_code\",\"token_url\":\"..\",\"auth_url\":\"..\",
     \"client_id\":\"{{client_id}}\",\"client_secret\":\"{{client_secret}}\",\"scope\":\"..\"}  (the token is obtained only when sending).
  Secrets ALWAYS go as {{variables}}; you do not see their values.
Writing scripts (pre or tests) or changing an auth always asks the user for approval: they touch their secrets.
Rules: requests that come from a CONTRACT (the repo's openapi) are edited only as the user's version; the contract is
read-only. Respect the contract's schema unless the user asks otherwise. Use {{variables}} instead of
sensitive values. Do only what the user asked and explain in one line what you are going to do before the block. For stress tests, propose
a battery suited to the request (smoke before load) and, when you receive the results, interpret latencies and errors in
plain language: which endpoint degrades, from how many users and what to check.";

/// SYSTEM plus the language it must reply in (the UI language).
pub fn system_prompt() -> String {
    let lang = if i18n::en() { "English" } else { "Spanish" };
    tr!(
        "{SYSTEM}\nIDIOMA: responde SIEMPRE en {lang} (el idioma de la interfaz), aunque el contexto esté en otro idioma.",
        "{SYSTEM}\nLANGUAGE: ALWAYS reply in {lang} (the UI language), even if the context is in another language.",
    )
}

/// messages: (role, content) with role "user" | "assistant". Blocking: call from a worker thread.
pub fn ask(
    client: &reqwest::blocking::Client,
    a: &AgentCfg,
    messages: &[(String, String)],
    claude_timeout: Duration,
) -> Result<String, String> {
    // The model is passed as a process argument: without this, "--something" would sneak in as a claude flag.
    let m = &a.model;
    if m.is_empty() || m.starts_with('-') || !m.chars().all(|c| c.is_ascii_alphanumeric() || "._:-/".contains(c)) {
        return Err(tr!("modelo inválido: {m:?}", "invalid model: {m:?}"));
    }
    match a.kind.as_str() {
        "claude" => claude(a, messages, claude_timeout),
        "ollama" => ollama(client, a, messages),
        k => Err(tr!("tipo de agente desconocido: {k:?} (usa claude u ollama)", "unknown agent kind: {k:?} (use claude or ollama)")),
    }
}

// claude -p in minimal mode: no tools, MCP, skills or CLAUDE.md; it only replies with text (~600 tokens of context).
// Runs in the temp directory so it does not load any project's CLAUDE.md.
fn claude(a: &AgentCfg, messages: &[(String, String)], timeout: Duration) -> Result<String, String> {
    let effort = if a.effort.is_empty() { "low" } else { &a.effort };
    if !["low", "medium", "high", "xhigh", "max"].contains(&effort) {
        return Err(tr!("effort inválido: {effort:?}", "invalid effort: {effort:?}"));
    }
    let prompt = messages
        .iter()
        .map(|(role, content)| format!("{}:\n{content}", if role == "assistant" { t("Asistente", "Assistant") } else { t("Usuario", "User") }))
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut cmd = Command::new("claude");
    cmd.args(["-p", "--output-format", "json", "--tools", "", "--strict-mcp-config", "--no-session-persistence"])
        .args(["--disable-slash-commands", "--setting-sources", "local"])
        .args(["--model", &a.model, "--effort", effort, "--system-prompt", &system_prompt()])
        .current_dir(std::env::temp_dir());
    let (stdout, stderr) = run_with_timeout(cmd, &prompt, timeout).map_err(|e| match e {
        RunError::Spawn(e) => tr!(
            "no se pudo ejecutar claude (¿está en el PATH?): {e}. Instala Claude Code e inicia sesión con `claude`, o elige Ollama en Configurar.",
            "could not run claude (is it on the PATH?): {e}. Install Claude Code and sign in with `claude`, or pick Ollama in Configure.",
        ),
        RunError::Timeout => tr!(
            "claude no respondió en {} s. Ajusta el timeout en Agente → Configurar.",
            "claude did not answer within {} s. Adjust the timeout in Agent → Configure.",
            timeout.as_secs()
        ),
        RunError::Io(e) => e,
    })?;

    let v: Value = serde_json::from_slice(&stdout).map_err(|_| {
        let err = String::from_utf8_lossy(if stderr.is_empty() { &stdout } else { &stderr });
        tr!("claude falló: {}", "claude failed: {}", err.trim())
    })?;
    let result = v["result"].as_str().unwrap_or_default().to_string();
    if v["is_error"].as_bool().unwrap_or(false) {
        return Err(format!("claude: {result}"));
    }
    Ok(result)
}

#[derive(Debug)]
enum RunError {
    Spawn(std::io::Error),
    Timeout,
    Io(String),
}

/// Runs `cmd` writing `input` to its stdin; kills it if it does not finish within `timeout`.
/// Returns (stdout, stderr). Reads both outputs on threads so a full pipe does not block it.
fn run_with_timeout(mut cmd: Command, input: &str, timeout: Duration) -> Result<(Vec<u8>, Vec<u8>), RunError> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no popup console
    }
    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    let reader = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            b
        })
    };
    let out = reader(Box::new(child.stdout.take().expect("stdout")));
    let err = reader(Box::new(child.stderr.take().expect("stderr")));
    // A process that does not read stdin must not break the call: the write error is ignored.
    let _ = child.stdin.take().expect("stdin").write_all(input.as_bytes());

    let deadline = Instant::now() + timeout;
    while child.try_wait().map_err(|e| RunError::Io(e.to_string()))?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok((out.join().unwrap_or_default(), err.join().unwrap_or_default()))
}

fn ollama(client: &reqwest::blocking::Client, a: &AgentCfg, messages: &[(String, String)]) -> Result<String, String> {
    let base = if a.url.is_empty() { "http://127.0.0.1:11434" } else { a.url.trim_end_matches('/') };
    let mut msgs = vec![json!({ "role": "system", "content": system_prompt() })];
    msgs.extend(messages.iter().map(|(role, content)| json!({ "role": role, "content": content })));
    let res = client
        .post(format!("{base}/api/chat"))
        .timeout(Duration::from_secs(300))
        .json(&json!({ "model": a.model, "messages": msgs, "stream": false }))
        .send()
        .map_err(err_chain)?;
    let status = res.status();
    let v: Value = res.json().map_err(err_chain)?;
    if !status.is_success() {
        return Err(format!("ollama {status}: {}", v["error"].as_str().unwrap_or("error")));
    }
    Ok(v["message"]["content"].as_str().unwrap_or_default().to_string())
}

/// Splits the reply by ```lang ... ``` blocks: (None, text) or (Some(language), code).
pub fn split_code(text: &str) -> Vec<(Option<String>, String)> {
    text.split("```")
        .enumerate()
        .filter_map(|(i, part)| {
            if i % 2 == 0 {
                let t = part.trim();
                (!t.is_empty()).then(|| (None, t.to_string()))
            } else {
                // The first line is the language if it is a single word ("js", "json", "actions"...).
                let (lang, code) = match part.split_once('\n') {
                    Some((lang, rest)) if !lang.trim().contains(' ') => (lang.trim(), rest),
                    _ => ("", part),
                };
                Some((Some(lang.to_string()), code.trim_end().to_string()))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_code_blocks() {
        let parts = split_code("Put this in Tests:\n```js\npm.test('a', () => {});\n```\nDone.");
        assert_eq!(
            parts,
            vec![
                (None, "Put this in Tests:".to_string()),
                (Some("js".to_string()), "pm.test('a', () => {});".to_string()),
                (None, "Done.".to_string())
            ]
        );
    }

    #[test]
    fn rejects_flag_like_models() {
        let a = AgentCfg { name: "x".into(), kind: "claude".into(), model: "--dangerously-skip-permissions".into(), effort: "".into(), url: "".into() };
        assert!(ask(&crate::infra::http::client(), &a, &[], Duration::from_secs(1)).unwrap_err().contains("modelo inválido"));
    }

    /// Calls the real Claude Haiku (uses quota): `cargo test --release -- --ignored haiku`
    #[test]
    #[ignore]
    fn haiku_follows_actions_protocol() {
        let a = AgentCfg { name: "h".into(), kind: "claude".into(), model: "claude-haiku-5-5".into(), effort: "low".into(), url: "".into() };
        let q = "Create the collection \"Demo\" with a request \"Ping\" GET to {{base}}/ping that has a status 200 test, \
                 create the environment \"qa\" with base=http://localhost:9 and activate it, and send the request.\n\n---\nContext:\nCollections: (none)\nEnvironments: (none)";
        let reply = ask(&crate::infra::http::client(), &a, &[("user".into(), q.into())], Duration::from_secs(120)).unwrap();
        let acts = crate::domain::actions::extract(&reply);
        println!("{reply}\n{acts:#?}");
        assert!(!acts.is_empty() && acts.iter().all(Result::is_ok), "invalid actions: {acts:?}");
        let acts: Vec<_> = acts.into_iter().map(Result::unwrap).collect();
        assert!(acts.iter().any(|x| matches!(x, crate::domain::actions::Action::CreateRequest { .. })));
        assert!(acts.iter().any(|x| matches!(x, crate::domain::actions::Action::CreateEnvironment { .. })));
    }

    /// Same as the previous one, for stress tests: `cargo test --release -- --ignored haiku`
    #[test]
    #[ignore]
    fn haiku_plans_stress_tests() {
        let a = AgentCfg { name: "h".into(), kind: "claude".into(), model: "claude-haiku-5-5".into(), effort: "low".into(), url: "".into() };
        let q = "Prepare a load test on Shop / List: ramp to 20 users in 10 s and then 1 minute sustained, \
                 with Login as setup. Save it as a battery to reuse it and run it.\n\n---\nContext:\nCollections:\n- Shop (2 requests)\n    POST Login\n    GET List\n\
                 Stress batteries: Smoke (1 VUs·10 s); Load (ramp→10 VUs·15 s → 10 VUs·60 s)";
        let reply = ask(&crate::infra::http::client(), &a, &[("user".into(), q.into())], Duration::from_secs(120)).unwrap();
        let acts = crate::domain::actions::extract(&reply);
        println!("{reply}\n{acts:#?}");
        assert!(!acts.is_empty() && acts.iter().all(Result::is_ok), "invalid actions: {acts:?}");
        use crate::domain::actions::Action;
        assert!(acts.iter().flatten().any(|x| matches!(x, Action::CreateBattery { stages, .. } if stages.len() == 2)));
        assert!(acts.iter().flatten().any(|x| matches!(x, Action::RunStress { setup: Some(s), .. } if s == "Login")));
    }

    #[cfg(windows)]
    #[test]
    fn kills_process_on_timeout() {
        let mut slow = Command::new("cmd");
        slow.args(["/C", "ping -n 10 127.0.0.1 >NUL"]);
        let t = Instant::now();
        assert!(matches!(run_with_timeout(slow, "", Duration::from_millis(500)), Err(RunError::Timeout)));
        assert!(t.elapsed() < Duration::from_secs(5));

        let mut fast = Command::new("cmd");
        fast.args(["/C", "findstr x"]); // echoes to stdout the stdin lines that contain "x"
        let (out, _) = run_with_timeout(fast, "xyz\nabc\n", Duration::from_secs(10)).unwrap();
        assert_eq!(String::from_utf8_lossy(&out).trim(), "xyz");
    }
}
