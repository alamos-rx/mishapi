//! `{{name}}` variables: parsing, resolution and dynamic variables (`{{$guid}}`, `{{$timestamp}}`, …).

use std::{
    collections::BTreeMap,
    hash::BuildHasher,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub type Vars = BTreeMap<String, String>;

pub fn parse_vars(text: &str) -> Vars {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !k.trim().is_empty())
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

pub fn format_vars(vars: &Vars) -> String {
    vars.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n")
}

/// "Key: value" per line; lines starting with "//" are disabled and not sent.
pub fn parse_headers(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .filter_map(|l| l.split_once(':'))
        .filter(|(k, _)| !k.trim().is_empty())
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Replaces {{var}}; missing ones are left as-is so it is noticeable.
pub fn resolve(text: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let inner = &after[..end];
        if inner.contains(['{', '}']) || inner.trim().is_empty() {
            out.push_str(&rest[..start + 1]);
            rest = &rest[start + 1..];
            continue;
        }
        out.push_str(&rest[..start]);
        match dynamic(inner.trim()).or_else(|| vars.get(inner.trim()).cloned()) {
            Some(v) => out.push_str(&v),
            None => out.push_str(&rest[start..start + 4 + end]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

fn dynamic(key: &str) -> Option<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    Some(match key {
        "$guid" => {
            let h = format!("{:016x}{:016x}", rand_u64(), rand_u64());
            format!("{}-{}-4{}-a{}-{}", &h[..8], &h[8..12], &h[13..16], &h[17..20], &h[20..])
        }
        "$timestamp" => now.as_secs().to_string(),
        "$isoTimestamp" => iso8601(now.as_secs() as i64, now.subsec_millis()),
        "$randomInt" => (rand_u64() % 1001).to_string(),
        _ => return None,
    })
}

// ponytail: randomness via std's RandomState; good enough for test ids, not for cryptography.
fn rand_u64() -> u64 {
    std::collections::hash_map::RandomState::new().hash_one(Instant::now())
}

/// Unix seconds -> "YYYY-MM-DDTHH:MM:SS.mmmZ" (H. Hinnant's civil_from_days algorithm).
pub fn iso8601(secs: i64, ms: u32) -> String {
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", sod / 3600, sod % 3600 / 60, sod % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_variables() {
        let vars: Vars = [("base".into(), "http://h".into()), ("id".into(), "7".into())].into();
        assert_eq!(resolve("{{base}}/u/{{ id }}?x={{nope}}", &vars), "http://h/u/7?x={{nope}}");
        assert_eq!(resolve("{{{id}}} {{", &vars), "{7} {{");
        let g = resolve("{{$guid}}", &vars);
        assert_eq!((g.len(), g.matches('-').count()), (36, 4));
    }


    #[test]
    fn parses_vars_and_dates() {
        let v = parse_vars("a=1\nb = x=y\ngarbage");
        assert_eq!(format_vars(&v), "a=1\nb=x=y");
        assert_eq!(iso8601(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_700_000_000, 5), "2023-11-14T22:13:20.005Z");
        assert_eq!(iso8601(951_782_400, 0), "2000-02-29T00:00:00.000Z");
    }
}
