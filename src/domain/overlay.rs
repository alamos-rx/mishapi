//! Two layers: the CONTRACT (what the openapi.yaml generates) and YOUR VERSION (what you edit).
//! Your "overlay" is the set of fields where your version differs from the contract. When the contract changes, a
//! per-field 3-way merge is done: where you touched nothing the new contract wins; where you changed something, yours wins
//! (even if the contract removed that field: it becomes "outside the contract" and validation flags it).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::{domain::model::ReqParts, tr};

type Flat = BTreeMap<Vec<String>, Value>;

/// A field where your version differs from the contract.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// "method", "path", "query.limit", "header.authorization", "body.customer.name" or "body" (non-JSON text)
    pub path: String,
    pub contract: Option<String>,
    pub yours: Option<String>,
}

// ---- Decomposition into fields ----

fn split_url(url: &str) -> (String, Vec<(String, String)>) {
    match url.split_once('?') {
        None => (url.to_string(), Vec::new()),
        Some((p, q)) => (
            p.to_string(),
            q.split('&').filter(|s| !s.is_empty()).map(|kv| kv.split_once('=').map_or((kv.to_string(), String::new()), |(k, v)| (k.into(), v.into()))).collect(),
        ),
    }
}

fn join_url(path: &str, query: &[(String, String)]) -> String {
    if query.is_empty() {
        return path.to_string();
    }
    let q: Vec<String> = query.iter().map(|(k, v)| if v.is_empty() { k.clone() } else { format!("{k}={v}") }).collect();
    format!("{path}?{}", q.join("&"))
}

fn headers(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).filter(|(k, _)| !k.is_empty()).collect()
}

/// JSON objects are traversed; arrays and scalars are leaves (ponytail: an array is merged whole, not per element).
fn flatten(v: &Value, prefix: &mut Vec<String>, out: &mut Flat) {
    match v {
        Value::Object(o) if !o.is_empty() => {
            for (k, x) in o {
                prefix.push(k.clone());
                flatten(x, prefix, out);
                prefix.pop();
            }
        }
        other => {
            out.insert(prefix.clone(), other.clone());
        }
    }
}

fn flat_json(body: &str) -> Option<Flat> {
    match serde_json::from_str::<Value>(body).ok()? {
        v @ Value::Object(_) => {
            let mut out = Flat::new();
            flatten(&v, &mut Vec::new(), &mut out);
            Some(out)
        }
        _ => None,
    }
}

/// Rebuilds the object. If a path collides with an already placed leaf (object <-> value type change), the first wins.
fn unflatten(flat: &Flat) -> Value {
    let mut root = Value::Object(Map::new());
    'outer: for (path, v) in flat {
        if path.is_empty() {
            return v.clone();
        }
        let mut node = &mut root;
        for seg in &path[..path.len() - 1] {
            let Value::Object(o) = node else { continue 'outer };
            node = o.entry(seg.clone()).or_insert_with(|| Value::Object(Map::new()));
        }
        if let Value::Object(o) = node {
            o.insert(path[path.len() - 1].clone(), v.clone());
        }
    }
    root
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

// ---- 3-way merge ----

/// Where `cur == old` (you did not touch it) `new` wins; otherwise `cur` wins.
fn pick<T: PartialEq + Clone>(old: Option<&T>, cur: Option<&T>, new: Option<&T>) -> Option<T> {
    if cur == old { new.cloned() } else { cur.cloned() }
}

fn merge_keyed(old: &[(String, String)], cur: &[(String, String)], new: &[(String, String)], key: fn(&str) -> String) -> Vec<(String, String)> {
    let find = |list: &[(String, String)], k: &str| list.iter().find(|(x, _)| key(x) == k).cloned();
    let mut keys: Vec<String> = Vec::new();
    for (k, _) in new.iter().chain(cur) {
        if !keys.contains(&key(k)) {
            keys.push(key(k));
        }
    }
    for (k, _) in old {
        if !keys.contains(&key(k)) {
            keys.push(key(k));
        }
    }
    keys.iter()
        .filter_map(|k| {
            let (o, c, n) = (find(old, k), find(cur, k), find(new, k));
            pick(o.as_ref().map(|x| &x.1), c.as_ref().map(|x| &x.1), n.as_ref().map(|x| &x.1))
                // The name (casing) comes from the contract if it has it.
                .map(|v| (n.or(c).or(o).map(|x| x.0).unwrap_or_else(|| k.clone()), v))
        })
        .collect()
}

fn ident(s: &str) -> String {
    s.to_string()
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

fn merge_body(old: &str, cur: &str, new: &str) -> String {
    match (flat_json(old), flat_json(cur), flat_json(new)) {
        (Some(o), Some(c), Some(n)) => {
            let mut keys: Vec<&Vec<String>> = o.keys().chain(c.keys()).chain(n.keys()).collect();
            keys.sort();
            keys.dedup();
            let merged: Flat = keys.into_iter().filter_map(|k| pick(o.get(k), c.get(k), n.get(k)).map(|v| (k.clone(), v))).collect();
            serde_json::to_string_pretty(&unflatten(&merged)).unwrap_or_default()
        }
        // Non-JSON body (or empty in some version): treated as a single value.
        _ => pick(Some(&old.to_string()), Some(&cur.to_string()), Some(&new.to_string())).unwrap_or_default(),
    }
}

/// Applies your overlay (what you changed relative to `old`) onto the new contract `new`.
pub fn rebase(old: &ReqParts, cur: &ReqParts, new: &ReqParts) -> ReqParts {
    let (op, oq) = split_url(&old.url);
    let (cp, cq) = split_url(&cur.url);
    let (np, nq) = split_url(&new.url);
    let hdr = |h: Vec<(String, String)>| h.into_iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
    ReqParts {
        method: pick(Some(&old.method), Some(&cur.method), Some(&new.method)).unwrap_or_default(),
        url: join_url(&pick(Some(&op), Some(&cp), Some(&np)).unwrap_or_default(), &merge_keyed(&oq, &cq, &nq, ident)),
        headers: hdr(merge_keyed(&headers(&old.headers), &headers(&cur.headers), &headers(&new.headers), lower)),
        body: merge_body(&old.body, &cur.body, &new.body),
    }
}

// ---- Diffs and restoring ----

pub fn diff(base: &ReqParts, cur: &ReqParts) -> Vec<Change> {
    let mut out = Vec::new();
    let mut push = |path: String, contract: Option<String>, yours: Option<String>| {
        if contract != yours {
            out.push(Change { path, contract, yours });
        }
    };
    push("method".into(), Some(base.method.clone()), Some(cur.method.clone()));
    let ((bp, bq), (cp, cq)) = (split_url(&base.url), split_url(&cur.url));
    push("path".into(), Some(bp), Some(cp));
    keyed_diff("query", &bq, &cq, ident, &mut push);
    keyed_diff("header", &headers(&base.headers), &headers(&cur.headers), lower, &mut push);
    match (flat_json(&base.body), flat_json(&cur.body)) {
        (Some(b), Some(c)) => {
            let mut keys: Vec<&Vec<String>> = b.keys().chain(c.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                push(format!("body.{}", k.join(".")), b.get(k).map(show), c.get(k).map(show));
            }
        }
        _ => push("body".into(), Some(base.body.clone()).filter(|s| !s.is_empty()), Some(cur.body.clone()).filter(|s| !s.is_empty())),
    }
    out
}

fn keyed_diff(prefix: &str, base: &[(String, String)], cur: &[(String, String)], key: fn(&str) -> String, push: &mut impl FnMut(String, Option<String>, Option<String>)) {
    let mut keys: Vec<String> = base.iter().chain(cur).map(|(k, _)| key(k)).collect();
    keys.dedup();
    let mut seen = Vec::new();
    for k in keys {
        if seen.contains(&k) {
            continue;
        }
        seen.push(k.clone());
        let get = |l: &[(String, String)]| l.iter().find(|(x, _)| key(x) == k).map(|(_, v)| v.clone());
        push(format!("{prefix}.{k}"), get(base), get(cur));
    }
}

/// Returns `cur` with field `path` set as in the contract (or removed if the contract lacks it).
pub fn restore(base: &ReqParts, cur: &ReqParts, path: &str) -> ReqParts {
    let mut out = cur.clone();
    let (bp, bq) = split_url(&base.url);
    let (cp, mut cq) = split_url(&cur.url);
    let set_keyed = |list: &mut Vec<(String, String)>, from: &[(String, String)], k: &str, key: fn(&str) -> String| {
        let wanted = from.iter().find(|(x, _)| key(x) == k).cloned();
        match (list.iter().position(|(x, _)| key(x) == k), wanted) {
            (Some(i), Some(w)) => list[i] = w,
            (Some(i), None) => {
                list.remove(i);
            }
            (None, Some(w)) => list.push(w),
            (None, None) => {}
        }
    };
    if path == "method" {
        out.method = base.method.clone();
    } else if path == "path" {
        out.url = join_url(&bp, &cq);
    } else if let Some(k) = path.strip_prefix("query.") {
        set_keyed(&mut cq, &bq, k, ident);
        out.url = join_url(&cp, &cq);
    } else if let Some(k) = path.strip_prefix("header.") {
        let mut h = headers(&cur.headers);
        set_keyed(&mut h, &headers(&base.headers), k, lower);
        out.headers = h.into_iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
    } else if path == "body" {
        out.body = base.body.clone();
    } else if let Some(k) = path.strip_prefix("body.") {
        if let (Some(b), Some(mut c)) = (flat_json(&base.body), flat_json(&cur.body)) {
            // ponytail: the path is split on "."; a JSON key containing "." cannot be restored on its own.
            let key: Vec<String> = k.split('.').map(String::from).collect();
            match b.get(&key) {
                Some(v) => c.insert(key, v.clone()),
                None => c.remove(&key),
            };
            out.body = serde_json::to_string_pretty(&unflatten(&c)).unwrap_or_default();
        }
    }
    out
}

// ---- Validation against the contract schema ----

/// Warnings (they do not block sending): types, enum, required and out-of-contract fields.
pub fn validate(body: &str, schema: &Value) -> Vec<String> {
    if schema.as_object().is_none_or(|o| o.is_empty()) || body.trim().is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(body) {
        Ok(v) => {
            let mut out = Vec::new();
            check(&v, schema, "body", &mut out);
            out.sort();
            out
        }
        Err(e) => vec![tr!("body: no es JSON válido ({e})", "body: not valid JSON ({e})")],
    }
}

fn check(v: &Value, s: &Value, path: &str, out: &mut Vec<String>) {
    for part in s["allOf"].as_array().into_iter().flatten() {
        check(v, part, path, out);
    }
    let types: Vec<&str> = match &s["type"] {
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
        Value::String(t) => vec![t.as_str()],
        _ => Vec::new(),
    };
    if v.is_null() && (s["nullable"] == true || types.contains(&"null")) {
        return;
    }
    if let Some(e) = s["enum"].as_array() {
        if !e.contains(v) {
            out.push(tr!("{path}: {} no está en {}", "{path}: {} is not in {}", show(v), Value::Array(e.clone())));
        }
    }
    let ok = |t: &str| match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        _ => true,
    };
    if !types.is_empty() && !types.iter().any(|t| ok(t)) {
        out.push(tr!("{path}: se esperaba {} y llegó {}", "{path}: expected {} but got {}", types.join("|"), show(v)));
        return;
    }
    if let (Some(o), Some(props)) = (v.as_object(), s["properties"].as_object()) {
        for r in s["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !o.contains_key(r) {
                out.push(tr!("{path}.{r}: es obligatorio y falta", "{path}.{r}: is required and missing"));
            }
        }
        for (k, x) in o {
            match props.get(k) {
                Some(ps) => check(x, ps, &format!("{path}.{k}"), out),
                None if s["additionalProperties"].is_object() || s["additionalProperties"] == true => {}
                None => out.push(tr!("{path}.{k}: fuera de contrato (no existe en el schema)", "{path}.{k}: outside the contract (not in the schema)")),
            }
        }
    }
    if let (Some(items), Some(arr)) = (s.get("items"), v.as_array()) {
        for (i, x) in arr.iter().enumerate() {
            check(x, items, &format!("{path}[{i}]"), out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parts(method: &str, url: &str, headers: &str, body: Value) -> ReqParts {
        ReqParts { method: method.into(), url: url.into(), headers: headers.into(), body: serde_json::to_string_pretty(&body).unwrap() }
    }

    #[test]
    fn rebase_applies_only_your_fields() {
        let old = parts("POST", "{{b}}/c?limit=10&page=1", "Accept: */*\nX-Old: 1", json!({"name": "string", "age": 0, "email": "e", "addr": {"city": "c", "zip": "z"}}));
        // your version: name, age, city, query page and a custom header; you deleted "zip"
        let cur = parts("POST", "{{b}}/c?limit=10&page=3", "accept: */*\nX-Old: 1\nX-Debug: 1", json!({"name": "Ana", "age": 30, "email": "x", "addr": {"city": "GT"}}));
        // new contract: email removed, new phone, limit 20, X-Old removed, v2 path
        let new = parts("POST", "{{b}}/v2/c?limit=20&page=1", "Accept: application/json", json!({"name": "string", "age": 0, "phone": "t", "addr": {"city": "c", "zip": "z", "country": "p"}}));

        let r = rebase(&old, &cur, &new);
        assert_eq!(r.url, "{{b}}/v2/c?limit=20&page=3");
        assert_eq!(r.headers, "Accept: application/json\nX-Debug: 1");
        let body: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(body, json!({"name": "Ana", "age": 30, "email": "x", "phone": "t", "addr": {"city": "GT", "country": "p"}}));
    }

    #[test]
    fn diff_restore_and_untouched() {
        let base = parts("GET", "/a?x=1", "A: 1", json!({"n": "s", "k": {"z": 1}}));
        let cur = parts("PUT", "/a?x=2&y=3", "A: 1\nB: 2", json!({"n": "Ana", "k": {"z": 1}, "extra": true}));
        let d = diff(&base, &cur);
        assert_eq!(d.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["method", "query.x", "query.y", "header.b", "body.extra", "body.n"]);
        assert_eq!((d[5].contract.as_deref(), d[5].yours.as_deref()), (Some("\"s\""), Some("\"Ana\"")));

        let r = restore(&base, &cur, "body.n");
        assert!(r.body.contains("\"s\"") && r.body.contains("extra"));
        let r = restore(&base, &r, "body.extra");
        assert!(!r.body.contains("extra"));
        assert_eq!(restore(&base, &cur, "query.y").url, "/a?x=2");
        assert_eq!(restore(&base, &cur, "header.b").headers, "A: 1");
        assert!(diff(&base, &base).is_empty());
        let r = rebase(&base, &base, &cur);
        assert!(diff(&cur, &r).is_empty(), "with no changes of yours, the new contract comes in whole");
    }

    #[test]
    fn validates_against_schema() {
        let schema = json!({
            "type": "object", "required": ["name", "age"],
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"},
                "status": {"type": "string", "enum": ["active", "inactive"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "note": {"type": "string", "nullable": true}
            }
        });
        let issues = validate(r#"{"age": "thirty", "status": "X", "tags": ["a", 1], "note": null, "email": "x"}"#, &schema);
        assert_eq!(
            issues,
            [
                "body.age: se esperaba integer y llegó \"thirty\"",
                "body.email: fuera de contrato (no existe en el schema)",
                "body.name: es obligatorio y falta",
                "body.status: \"X\" no está en [\"active\",\"inactive\"]",
                "body.tags[1]: se esperaba string y llegó 1",
            ]
        );
        assert!(validate(r#"{"name": "a", "age": 1}"#, &schema).is_empty());
        assert!(validate("{nope", &schema)[0].starts_with("body: no es JSON"));
        assert!(validate("{}", &Value::Null).is_empty());
    }
}
