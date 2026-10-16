//! Runs the user's pre-request / test scripts with Boa (a JavaScript engine written in Rust).
//! Rust passes the context as JSON and gets back variables, the modified request and results.

use boa_engine::{Context, JsArgs, JsResult, JsString, JsValue, NativeFunction, Source, js_string};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use crate::domain::model::ReqParts;
use crate::{
    domain::{model::Response, vars::Vars},
    infra::http,
    tr,
};

#[derive(Clone, Default, Debug)]
pub struct Scopes {
    pub local: Vars,
    pub env: Vars,
    pub collection: Vars,
    /// Data-file row for the runner's current iteration (read-only).
    pub data: Vars,
}

impl Scopes {
    /// Precedence: request (local) > iteration data > environment > collection.
    pub fn merged(&self) -> Vars {
        let mut v = self.collection.clone();
        v.extend(self.env.clone());
        v.extend(self.data.clone());
        v.extend(self.local.clone());
        v
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TestResult {
    pub name: String,
    pub pass: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Outcome {
    pub tests: Vec<TestResult>,
    pub error: Option<String>,
    pub logs: Vec<String>,
    /// pm.setNextRequest: Some(Some(name)) jumps to that request, Some(None) ends the iteration.
    pub next: Option<Option<String>>,
}

const LOOP_LIMIT: u64 = 50_000_000; // a while(true) in a script does not hang the app

pub fn run(code: &str, scopes: &mut Scopes, req: &mut ReqParts, resp: Option<&Response>) -> Outcome {
    if code.trim().is_empty() {
        return Outcome::default();
    }
    let input = json!({
        "local": scopes.local, "env": scopes.env, "collection": scopes.collection, "data": scopes.data,
        "request": req, "response": resp,
    });
    // JSON is a valid JS literal: the user's code goes in as a string, it is not interpolated.
    // `__en` picks the language of the assertion messages, which are shown in the UI.
    let src = format!("const __en = {};\n{PRELUDE}\n__run({input}, {});", crate::i18n::en(), serde_json::Value::from(code));

    let mut ctx = Context::default();
    ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_LIMIT);
    let out = ctx
        .register_global_callable(js_string!("__http"), 1, NativeFunction::from_fn_ptr(http_native))
        .and_then(|_| ctx.eval(Source::from_bytes(&src)))
        .and_then(|_| {
            // Runs pending .then / await jobs (e.g. pm.sendRequest used as a promise) before reading the state.
            let _ = ctx.run_jobs();
            ctx.eval(Source::from_bytes("__finish()"))
        })
        .and_then(|v| v.to_string(&mut ctx));
    let out = match out {
        Ok(s) => s.to_std_string_escaped(),
        Err(e) => return Outcome { error: Some(e.to_string()), ..Default::default() },
    };

    #[derive(Deserialize)]
    struct Out {
        local: Vars,
        env: Vars,
        collection: Vars,
        request: ReqParts,
        tests: Vec<TestResult>,
        error: Option<String>,
        logs: Vec<String>,
        #[serde(default)]
        next: Option<Next>,
    }
    #[derive(Deserialize)]
    struct Next {
        name: Option<String>,
    }
    match serde_json::from_str::<Out>(&out) {
        Ok(o) => {
            (scopes.local, scopes.env, scopes.collection) = (o.local, o.env, o.collection);
            *req = o.request;
            Outcome { tests: o.tests, error: o.error, logs: o.logs, next: o.next.map(|n| n.name) }
        }
        Err(e) => Outcome { error: Some(tr!("salida inválida del script: {e}", "invalid script output: {e}")), ..Default::default() },
    }
}

/// `__http(json)` for pm.sendRequest: takes {method, url, headers, body} and returns {response} or {error}.
/// It blocks, but scripts always run on the send worker thread, never on the UI thread.
fn http_native(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    #[derive(Deserialize)]
    struct In {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: String,
    }
    let input = args.get_or_undefined(0).to_string(ctx)?.to_std_string_escaped();
    let out = match serde_json::from_str::<In>(&input) {
        Ok(r) => match http::send_http(&http::client(), &r.method, &r.url, &r.headers, &r.body) {
            Ok(resp) => json!({ "response": resp }),
            Err(e) => json!({ "error": e }),
        },
        Err(e) => json!({ "error": tr!("pm.sendRequest: petición inválida: {e}", "pm.sendRequest: invalid request: {e}") }),
    };
    Ok(JsValue::from(JsString::from(out.to_string().as_str())))
}

/// Script pm.* API (compatible with that of imported collections). It is JavaScript because it runs inside Boa next to the user's script.
const PRELUDE: &str = r#"
let __finish;
function __run(ctx, code) {
  const logs = [], tests = [];
  const L = (es, en) => __en ? en : es;
  const has = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
  const str = v => typeof v === 'string' ? v : JSON.stringify(v);
  const merged = () => Object.assign({}, ctx.collection, ctx.env, ctx.data, ctx.local);
  const hex = n => { let s = ''; for (let i = 0; i < n; i++) s += (Math.random() * 16 | 0).toString(16); return s; };
  const dyn = {
    $guid: () => `${hex(8)}-${hex(4)}-4${hex(3)}-a${hex(3)}-${hex(12)}`,
    $timestamp: () => String(Math.floor(Date.now() / 1000)),
    $isoTimestamp: () => new Date().toISOString(),
    $randomInt: () => String(Math.floor(Math.random() * 1001)),
  };
  const resolve = (s, vars) => String(s == null ? '' : s).replace(/\{\{\s*([^{}]+?)\s*\}\}/g,
    (m, k) => has(dyn, k) ? dyn[k]() : has(vars, k) ? vars[k] : m);

  const scope = o => ({
    get: k => o[k],
    set: (k, v) => { o[k] = str(v); },
    unset: k => { delete o[k]; },
    has: k => has(o, k),
    clear: () => { for (const k of Object.keys(o)) delete o[k]; },
    toObject: () => Object.assign({}, o),
  });

  function expect(v) {
    const j = x => JSON.stringify(x);
    let a;
    const check = (ok, msg) => { if (!ok) throw new Error(msg); return a; };
    a = {
      equal: x => check(v === x, L(`se esperaba ${j(x)} y llegó ${j(v)}`, `expected ${j(x)} but got ${j(v)}`)),
      eql: x => check(j(v) === j(x), L(`se esperaba ${j(x)} y llegó ${j(v)}`, `expected ${j(x)} but got ${j(v)}`)),
      include: x => check(
        typeof v === 'string' || Array.isArray(v) ? v.includes(x)
          : v != null && Object.keys(x).every(k => j(v[k]) === j(x[k])),
        L(`${j(v)} no incluye ${j(x)}`, `${j(v)} does not include ${j(x)}`)),
      above: x => check(v > x, L(`${j(v)} no es mayor que ${j(x)}`, `${j(v)} is not above ${j(x)}`)),
      below: x => check(v < x, L(`${j(v)} no es menor que ${j(x)}`, `${j(v)} is not below ${j(x)}`)),
      property: k => check(v != null && k in Object(v), L(`falta la propiedad ${j(k)}`, `missing property ${j(k)}`)),
      lengthOf: n => check(v != null && v.length === n, L(`largo ${v && v.length}, se esperaba ${n}`, `length ${v && v.length}, expected ${n}`)),
      a: t => check(t === 'array' ? Array.isArray(v) : typeof v === t, L(`${j(v)} no es ${t}`, `${j(v)} is not ${t}`)),
      status: c => check(v.code === c, L(`status ${v.code}, se esperaba ${c}`, `status ${v.code}, expected ${c}`)),
      get true() { return check(v === true, L(`${j(v)} no es true`, `${j(v)} is not true`)); },
      get false() { return check(v === false, L(`${j(v)} no es false`, `${j(v)} is not false`)); },
      get ok() { return check(!!v, L(`${j(v)} no es verdadero`, `${j(v)} is not truthy`)); },
      get exist() { return check(v != null, L('el valor no existe', 'the value does not exist')); },
      get empty() { return check(v != null && Object.keys(v).length === 0, L(`${j(v)} no está vacío`, `${j(v)} is not empty`)); },
    };
    a.an = a.a;
    a.equals = a.eq = a.equal;
    a.deep = { equal: a.eql, eql: a.eql };
    a.to = a.be = a.been = a.have = a.that = a.is = a.and = a;
    return a;
  }

  // pm.request: url/method as strings, headers with the pm.* API, body as a string or {raw, update()}.
  const r = ctx.request;
  const list = r.headers.split('\n').map(l => {
    const i = l.indexOf(':');
    return i > 0 ? { key: l.slice(0, i).trim(), value: l.slice(i + 1).trim() } : null;
  }).filter(Boolean);
  const find = k => list.findIndex(h => h.key.toLowerCase() === String(k).toLowerCase());
  const headers = {
    add: h => { list.push({ key: h.key, value: String(h.value) }); },
    upsert: h => { const i = find(h.key); if (i < 0) headers.add(h); else list[i].value = String(h.value); },
    remove: k => { for (let i = find(k); i >= 0; i = find(k)) list.splice(i, 1); },
    get: k => { const i = find(k); return i < 0 ? undefined : list[i].value; },
    has: k => find(k) >= 0,
    toObject: () => { const o = {}; for (const h of list) o[h.key] = h.value; return o; },
    all: () => list.map(h => ({ key: h.key, value: h.value })),
  };
  const request = {
    method: r.method, url: r.url, headers,
    body: { raw: r.body, update(x) { this.raw = str(x); }, toString() { return this.raw; } },
  };

  const makeResponse = res => {
    const out = {
      code: res.status, status: res.status_text, responseTime: res.ms,
      headers: { get: k => { const h = res.headers.find(p => p[0].toLowerCase() === String(k).toLowerCase()); return h && h[1]; } },
      text: () => res.body,
      json: () => JSON.parse(res.body),
    };
    out.to = expect(out); // pm.response.to.have.status(200)
    return out;
  };
  const response = ctx.response ? makeResponse(ctx.response) : undefined;

  // pm.sendRequest(url | {url, method, header, body}, (err, res) => …) or as a promise (.then / await).
  // Does not resolve {{vars}} by itself: use pm.variables.replaceIn(...).
  const toReq = q => {
    if (typeof q === 'string') return { method: 'GET', url: q, headers: [], body: '' };
    const h = q.header || q.headers;
    const headers = Array.isArray(h) ? h.filter(x => !x.disabled).map(x => [String(x.key), String(x.value)])
      : h ? Object.keys(h).map(k => [k, String(h[k])]) : [];
    const b = q.body;
    let body = '';
    if (typeof b === 'string') body = b;
    else if (b && b.mode === 'raw') body = String(b.raw == null ? '' : b.raw);
    else if (b && b.mode === 'urlencoded') {
      body = (b.urlencoded || []).filter(x => !x.disabled)
        .map(x => `${encodeURIComponent(x.key)}=${encodeURIComponent(x.value == null ? '' : x.value)}`).join('&');
      if (!headers.some(p => p[0].toLowerCase() === 'content-type')) headers.push(['Content-Type', 'application/x-www-form-urlencoded']);
    }
    return { method: String(q.method || 'GET').toUpperCase(), url: String(q.url), headers, body };
  };
  const sendRequest = (q, cb) => {
    let err = null, res = null;
    try {
      const out = JSON.parse(__http(JSON.stringify(toReq(q))));
      if (out.error) err = new Error(out.error); else res = makeResponse(out.response);
    } catch (e) { err = e; }
    if (cb) { cb(err, res); return; }
    return err ? Promise.reject(err) : Promise.resolve(res);
  };

  const local = scope(ctx.local);
  let next;
  const pm = {
    environment: scope(ctx.env),
    collectionVariables: scope(ctx.collection),
    globals: scope(ctx.env), // no global scope of its own: goes to the environment
    variables: Object.assign({}, local, {
      get: k => merged()[k],
      has: k => has(merged(), k),
      toObject: merged,
      replaceIn: s => resolve(s, merged()),
    }),
    iterationData: {
      get: k => ctx.data[k],
      has: k => has(ctx.data, k),
      toObject: () => Object.assign({}, ctx.data),
    },
    // null ends the iteration; a name jumps to that request (runner only).
    setNextRequest: n => { next = { name: n == null ? null : String(n) }; },
    request, response, expect, sendRequest,
    test(name, fn) {
      try { fn(); tests.push({ name, pass: true }); }
      catch (e) { tests.push({ name, pass: false, error: msg(e) }); }
    },
  };
  const out = (...a) => logs.push(a.map(x => typeof x === 'string' ? x : JSON.stringify(x)).join(' '));
  const console = { log: out, info: out, warn: out, error: out };

  // Async function to allow await; errors (including promise rejections) end up in `error`.
  let error = null;
  const AsyncFunction = (async () => {}).constructor;
  try { new AsyncFunction('pm', 'console', code)(pm, console).catch(e => { error = msg(e); }); }
  catch (e) { error = msg(e); }

  __finish = () => {
    const b = request.body;
    const strs = o => { const x = {}; for (const k of Object.keys(o)) x[k] = str(o[k]); return x; };
    return JSON.stringify({
      local: strs(ctx.local), env: strs(ctx.env), collection: strs(ctx.collection),
      request: {
        method: String(request.method).toUpperCase(), url: String(request.url),
        headers: list.map(h => `${h.key}: ${h.value}`).join('\n'),
        body: typeof b === 'string' ? b : String(b.raw),
      },
      tests, error, logs, next,
    });
  };
}
function msg(e) { return String(e && e.message || e); }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(p: &[(&str, &str)]) -> Vars {
        p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn scopes_and_request_api() {
        let mut s = Scopes {
            env: vars(&[("a", "env"), ("shared", "env")]),
            collection: vars(&[("c", "col"), ("shared", "col")]),
            ..Default::default()
        };
        let mut req = ReqParts { method: "get".into(), url: "{{base}}/x".into(), headers: "Accept: */*\nX-Old: 1".into(), body: "".into() };
        let o = run(
            r#"
            pm.variables.set('local', 'L');
            pm.environment.set('fromPre', { n: 1 });
            pm.collectionVariables.set('colNew', 'C2');
            pm.collectionVariables.unset('c');
            pm.request.headers.add({ key: 'X-Id', value: pm.variables.get('local') });
            pm.request.headers.upsert({ key: 'accept', value: 'application/json' });
            pm.request.headers.remove('X-Old');
            pm.request.body.update({ shared: pm.variables.get('shared') });
            pm.request.url = pm.variables.replaceIn('{{shared}}/y');
            pm.request.method = 'post';
            console.log('hello', { x: 1 });
            "#,
            &mut s,
            &mut req,
            None,
        );
        assert_eq!(o.error, None);
        assert_eq!(o.logs, vec![r#"hello {"x":1}"#]);
        assert_eq!(s.local, vars(&[("local", "L")]));
        assert_eq!(s.env, vars(&[("a", "env"), ("shared", "env"), ("fromPre", r#"{"n":1}"#)]));
        assert_eq!(s.collection, vars(&[("shared", "col"), ("colNew", "C2")]));
        assert_eq!(
            req,
            ReqParts {
                method: "POST".into(),
                url: "env/y".into(),
                headers: "Accept: application/json\nX-Id: L".into(),
                body: r#"{"shared":"env"}"#.into()
            }
        );
        assert_eq!(s.merged().get("shared").map(String::as_str), Some("env"));
    }

    #[test]
    fn tests_against_response() {
        let mut s = Scopes { local: vars(&[("local", "L")]), ..Default::default() };
        let mut req = ReqParts::default();
        let resp = Response {
            status: 200,
            status_text: "OK".into(),
            ms: 5,
            size: 0,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: r#"{"id":3,"tags":["x"],"token":"T","ok":true}"#.into(),
        };
        let o = run(
            r#"
            pm.test('status', () => pm.response.to.have.status(200));
            pm.test('id', () => pm.expect(pm.response.json().id).to.equal(3));
            pm.test('deep', () => pm.expect(pm.response.json().tags).to.deep.equal(['x']));
            pm.test('header', () => pm.expect(pm.response.headers.get('content-type')).to.include('json'));
            pm.test('getter', () => pm.expect(pm.response.json().ok).to.be.true);
            pm.test('local', () => pm.expect(pm.variables.get('local')).to.equal('L'));
            pm.test('fails', () => pm.expect(1).to.be.above(2));
            pm.environment.set('token', pm.response.json().token);
            "#,
            &mut s,
            &mut req,
            Some(&resp),
        );
        assert_eq!(o.error, None);
        assert_eq!(o.tests.iter().map(|t| t.pass).collect::<Vec<_>>(), [true, true, true, true, true, true, false]);
        assert_eq!(o.tests[6].error.as_deref(), Some("1 no es mayor que 2"));
        assert_eq!(s.env.get("token").map(String::as_str), Some("T"));
    }

    /// HTTP server that echoes back, as JSON, the first line and the body it received (`hits` connections).
    fn echo_server(hits: usize) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(hits) {
                let mut s = stream.unwrap();
                let (mut raw, mut buf) = (Vec::new(), [0; 4096]);
                // Read until the headers are complete and the whole body (Content-Length) is in.
                let req = loop {
                    let n = s.read(&mut buf).unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    let t = String::from_utf8_lossy(&raw).to_string();
                    if let Some(i) = t.find("\r\n\r\n") {
                        let len = t
                            .lines()
                            .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                            .unwrap_or(0);
                        if n == 0 || t.len() >= i + 4 + len {
                            break t;
                        }
                    } else if n == 0 {
                        break t;
                    }
                };
                let line = req.lines().next().unwrap_or("").to_string();
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                let out = json!({ "line": line, "body": body }).to_string();
                write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len()).unwrap();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn send_request_callback_and_promise() {
        let base = echo_server(2);
        let mut s = Scopes { env: vars(&[("base", &base)]), ..Default::default() };
        let mut req = ReqParts::default();
        let o = run(
            r#"
            pm.sendRequest({
              url: pm.variables.replaceIn('{{base}}/login'), method: 'post',
              header: [{ key: 'Content-Type', value: 'application/json' }],
              body: { mode: 'raw', raw: JSON.stringify({ u: 'ana' }) },
            }, (err, res) => {
              pm.test('callback', () => pm.expect(res.json().line).to.equal('POST /login HTTP/1.1'));
              pm.environment.set('token', JSON.parse(res.json().body).u);
            });
            const res = await pm.sendRequest(pm.environment.get('base') + '/ping');
            pm.test('await', () => res.to.have.status(200));
            pm.sendRequest('http://127.0.0.1:1/nothing').catch(e => pm.environment.set('failed', 'yes'));
            "#,
            &mut s,
            &mut req,
            None,
        );
        assert_eq!(o.error, None);
        assert_eq!(o.tests.iter().map(|t| (t.name.as_str(), t.pass)).collect::<Vec<_>>(), [("callback", true), ("await", true)]);
        assert_eq!(s.env.get("token").map(String::as_str), Some("ana"));
        assert_eq!(s.env.get("failed").map(String::as_str), Some("yes"));
    }

    #[test]
    fn errors_and_infinite_loops_are_contained() {
        let mut s = Scopes::default();
        let mut req = ReqParts { method: "GET".into(), url: "u".into(), ..Default::default() };
        let o = run(r#"pm.request.url = "changed"; throw new Error("boom")"#, &mut s, &mut req, None);
        assert_eq!(o.error.as_deref(), Some("boom"));
        assert_eq!(req.url, "changed");
        let o = run("while (true) {}", &mut s, &mut req, None);
        assert!(o.error.is_some());
    }
}
