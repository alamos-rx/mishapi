//! Domain entities: collections, requests, authorization, environments, agents and stress batteries.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use super::vars::Vars;

/// Application preferences.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub claude_timeout_secs: u64,
    /// Recently used Azure DevOps repos.
    pub repos: Vec<RepoCfg>,
    /// Stress batteries (editable in settings.json).
    pub batteries: Vec<Battery>,
    /// Tabs open when the app closed: (collection, request), to restore them.
    pub open_tabs: Vec<(String, String)>,
    pub active_tab: usize,
    /// One Light theme instead of One Dark.
    pub light_theme: bool,
    /// Agent panel open (yes by default: the app is agentic first).
    pub agent_panel: bool,
    /// Language of the UI, the CLI and the agent's replies.
    pub language: crate::i18n::Language,
}

impl Default for Settings {
    fn default() -> Self {
        Self { claude_timeout_secs: 120, repos: Vec::new(), batteries: default_batteries(), open_tabs: Vec::new(), active_tab: 0, light_theme: false, agent_panel: true, language: Default::default() }
    }
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepoCfg {
    pub url: String,
    #[serde(default)]
    pub branch: String,
}

/// What comes from the repo and is compared when syncing.
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReqParts {
    pub method: String,
    pub url: String,
    pub headers: String,
    pub body: String,
}

/// Endpoint that came from a repo. `base` is the CONTRACT at the last sync (read-only);
/// the request fields are YOUR VERSION. `key` identifies it ("GET /users/{id}").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub key: String,
    pub base: ReqParts,
    /// JSON schema of the body per the contract ($refs already resolved), to validate your version.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub schema: serde_json::Value,
}

/// Repo file a collection comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub repo: String,
    #[serde(default)]
    pub branch: String,
    pub file: String,
    #[serde(default)]
    pub commit: String,
}

/// Authorization of a request or a collection. Applied when sending, after the pre-request.
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    /// On a request: uses its collection's. On a collection: no auth.
    #[default]
    Inherit,
    None,
    Bearer { token: String },
    Basic { user: String, pass: String },
    ApiKey { key: String, value: String, #[serde(default)] in_query: bool },
    Oauth2(OAuth2),
}

impl Auth {
    pub fn is_inherit(&self) -> bool {
        *self == Auth::Inherit
    }
}

/// OAuth 2.0. Fields accept {{vars}}. The obtained token is stored here to reuse it until it expires.
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuth2 {
    /// client_credentials | password | authorization_code
    pub grant: String,
    pub token_url: String,
    pub auth_url: String,
    /// Must be registered with the provider; the app listens there for the browser's redirect.
    pub callback_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scope: String,
    pub username: String,
    pub password: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds; 0 = no known expiry.
    pub expires_at: u64,
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    pub name: String,
    pub method: String,
    pub url: String,
    pub headers: String,
    pub body: String,
    pub pre: String,
    pub tests: String,
    #[serde(skip_serializing_if = "Auth::is_inherit")]
    pub auth: Auth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

impl Request {
    pub fn parts(&self) -> ReqParts {
        ReqParts { method: self.method.clone(), url: self.url.clone(), headers: self.headers.clone(), body: self.body.clone() }
    }

    pub fn set_parts(&mut self, p: ReqParts) {
        (self.method, self.url, self.headers, self.body) = (p.method, p.url, p.headers, p.body);
    }

    /// Came from the repo and was changed locally since the last sync.
    pub fn modified(&self) -> bool {
        self.origin.as_ref().is_some_and(|o| o.base != self.parts())
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    pub name: String,
    pub variables: Vars,
    pub requests: Vec<Request>,
    #[serde(skip_serializing_if = "Auth::is_inherit")]
    pub auth: Auth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
}

/// Moves a request (drag and drop). `to_idx` is the target position computed with the request still in
/// place. Returns how each position maps afterwards (to fix up open tabs), or None if it did not move.
pub fn move_request(cols: &mut [Collection], from: (usize, usize), to_col: usize, to_idx: usize) -> Option<impl Fn((usize, usize)) -> (usize, usize) + use<>> {
    if from.1 >= cols.get(from.0)?.requests.len() || to_col >= cols.len() {
        return None;
    }
    let to_idx = if from.0 == to_col && to_idx > from.1 { to_idx - 1 } else { to_idx }.min(cols[to_col].requests.len() - usize::from(from.0 == to_col));
    if (to_col, to_idx) == from {
        return None;
    }
    let req = cols[from.0].requests.remove(from.1);
    cols[to_col].requests.insert(to_idx, req);
    Some(move |(c, i): (usize, usize)| {
        if (c, i) == from {
            return (to_col, to_idx);
        }
        let i = if c == from.0 && i > from.1 { i - 1 } else { i };
        (c, if c == to_col && i >= to_idx { i + 1 } else { i })
    })
}

/// Copy of the request with the auth that actually applies (its own or inherited from the collection).
pub fn with_effective_auth(r: &Request, col: Option<&Collection>) -> Request {
    let mut r = r.clone();
    if r.auth.is_inherit() {
        r.auth = col.map(|c| c.auth.clone()).filter(|a| !a.is_inherit()).unwrap_or(Auth::None);
    }
    r
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Environments {
    pub active: String,
    pub envs: BTreeMap<String, Vars>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentCfg {
    pub name: String,
    pub kind: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effort: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}

pub fn default_agents() -> Vec<AgentCfg> {
    let a = |name: &str, kind: &str, model: &str, effort: &str, url: &str| AgentCfg {
        name: name.into(),
        kind: kind.into(),
        model: model.into(),
        effort: effort.into(),
        url: url.into(),
    };
    vec![
        a("Claude Haiku", "claude", "claude-haiku-5-5", "low", ""),
        a("Ollama local", "ollama", "llama3.2", "", "http://127.0.0.1:11434"),
    ]
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct Response {
    pub status: u16,
    pub status_text: String,
    pub ms: u64,
    pub size: usize,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

// ---- Stress ----

/// A stage: `vus` virtual users in parallel for `secs` seconds, with no pause between requests.
/// With `ramp`, users rise (or fall) linearly from the previous stage's count (1 if it is the first).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub vus: u32,
    pub secs: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ramp: bool,
}

impl Stage {
    /// Active users `t` seconds into the stage.
    pub fn vus_at(&self, from: u32, t: f64) -> u32 {
        if !self.ramp {
            return self.vus;
        }
        let k = (t / self.secs.max(1) as f64).clamp(0.0, 1.0);
        ((from as f64 + (self.vus as f64 - from as f64) * k).round() as u32).max(1)
    }

    pub fn label(&self) -> String {
        if self.ramp { crate::tr!("rampa→{} VUs·{} s", "ramp→{} VUs·{} s", self.vus, self.secs) } else { format!("{} VUs·{} s", self.vus, self.secs) }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Battery {
    pub name: String,
    pub description: String,
    pub stages: Vec<Stage>,
}

pub const MAX_VUS: u32 = 500;

pub fn default_batteries() -> Vec<Battery> {
    use crate::i18n::t;
    // (vus, seconds, ramp)
    let b = |name: &str, description: &str, stages: &[(u32, u64, bool)]| Battery {
        name: name.into(),
        description: description.into(),
        stages: stages.iter().map(|&(vus, secs, ramp)| Stage { vus, secs, ramp }).collect(),
    };
    vec![
        b(t("Humo", "Smoke"), t("1 usuario, 10 s: ¿responde y en cuánto?", "1 user, 10 s: does it respond, and how fast?"), &[(1, 10, false)]),
        b(t("Carga", "Load"), t("Rampa a 10 usuarios y 60 s sostenidos: tráfico normal", "Ramp to 10 users and 60 s sustained: normal traffic"), &[(10, 15, true), (10, 60, false)]),
        b(
            t("Estrés", "Stress"),
            t("Escalera con rampas 10→50→100 usuarios: ¿dónde se degrada?", "Staircase with ramps 10→50→100 users: where does it degrade?"),
            &[(10, 10, true), (10, 20, false), (50, 20, true), (50, 20, false), (100, 20, true), (100, 20, false)],
        ),
        b(t("Pico", "Spike"), t("1→100→1 usuarios de golpe: ráfaga repentina y recuperación", "1→100→1 users at once: sudden burst and recovery"), &[(1, 10, false), (100, 15, false), (1, 10, false)]),
        b(t("Rampa", "Ramp"), t("Sube de 1 a 100 usuarios en 2 min: encuentra el punto de quiebre", "Goes from 1 to 100 users in 2 min: finds the breaking point"), &[(100, 120, true)]),
        b(t("Resistencia", "Soak"), t("Rampa a 20 usuarios y 10 min sostenidos: fugas de memoria / conexiones", "Ramp to 20 users and 10 min sustained: memory / connection leaks"), &[(20, 30, true), (20, 600, false)]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_requests_and_remaps_positions() {
        let col = |n: &str, reqs: &[&str]| Collection {
            name: n.into(),
            requests: reqs.iter().map(|r| Request { name: r.to_string(), ..Default::default() }).collect(),
            ..Default::default()
        };
        let names = |cols: &[Collection]| cols.iter().map(|c| c.requests.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(",")).collect::<Vec<_>>();
        let mut cols = vec![col("A", &["a0", "a1", "a2", "a3"]), col("B", &["b0"])];
        // a3 above a0 (drop on the upper half of a0)
        let m = move_request(&mut cols, (0, 3), 0, 0).unwrap();
        assert_eq!(names(&cols), ["a3,a0,a1,a2", "b0"]);
        assert_eq!((m((0, 3)), m((0, 0)), m((0, 2)), m((1, 0))), ((0, 0), (0, 1), (0, 3), (1, 0)));
        // a3 below a1 (drop on the lower half of a1 = position 3 with a3 still at 0)
        let m = move_request(&mut cols, (0, 0), 0, 3).unwrap();
        assert_eq!(names(&cols), ["a0,a1,a3,a2", "b0"]);
        assert_eq!((m((0, 0)), m((0, 1)), m((0, 3))), ((0, 2), (0, 0), (0, 3)));
        // into another collection, at the end (drop on its title)
        let m = move_request(&mut cols, (0, 1), 1, usize::MAX).unwrap();
        assert_eq!(names(&cols), ["a0,a3,a2", "b0,a1"]);
        assert_eq!((m((0, 1)), m((0, 2)), m((1, 0))), ((1, 1), (0, 1), (1, 0)));
        assert!(move_request(&mut cols, (0, 1), 0, 1).is_none() && move_request(&mut cols, (0, 1), 0, 2).is_none(), "dropping in the same place");
        assert!(move_request(&mut cols, (5, 0), 0, 0).is_none());
    }
}
