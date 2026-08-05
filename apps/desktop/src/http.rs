//! HTTP client core behind the agent's `http_request` verb and the Services view's
//! HTTP summary (C1 foundation; the Postman-style center-tab builder is C2).
//!
//! Two concerns, both **GPUI-free** so they unit-test without a UI or a runtime:
//! - **State**: [`HttpHistory`] — a shared, capped ring of [`HttpCall`] records (the
//!   Services summary polls it; the executor records into it), mirroring
//!   [`RunRegistry`](crate::run::RunRegistry)'s shared-handle shape.
//! - **Logic**: the variable **manifest** (`.moonlight/http/environments.json`),
//!   `{{var}}` interpolation, host-scoping (the SSRF gate), payload parsing, and the
//!   blocking [`send`] (ureq). The executor runs `send` on `spawn_blocking`.
//!
//! Host-scoping is the security gate: an environment may list `allowed_hosts`; with
//! none listed, only loopback/private hosts are reachable (so a default manifest can't
//! be turned into an exfiltration channel). Widening to a public host is a manifest
//! edit — a deliberate operator act.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Cap on retained history records — oldest drop off the front.
const HISTORY_CAP: usize = 50;
/// Body preview kept in a history record / returned to the agent.
const BODY_PREVIEW: usize = 600;
/// Body kept for the HTTP panel's response pane (the operator reads more than the
/// agent's compact preview, but still bounded so a huge response can't blow memory).
const PANEL_BODY: usize = 200_000;
/// Per-request timeout for the blocking client.
const TIMEOUT: Duration = Duration::from_secs(15);

/// One completed (or failed) HTTP call, as the history shows it. Deliberately omits
/// request headers/body so a bearer token in a header never lands in the ring.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HttpCall {
    pub method: String,
    /// The interpolated URL actually requested.
    pub url: String,
    /// HTTP status, when a response came back (`None` = transport error).
    pub status: Option<u16>,
    pub ms: u64,
    pub ok: bool,
    pub bytes: usize,
    pub at_millis: i64,
    /// Transport/usage error message, when the call didn't yield a response.
    pub error: Option<String>,
}

struct Inner {
    calls: std::collections::VecDeque<HttpCall>,
}

/// Thread-safe handle to the HTTP call history. Cheap to clone; every clone sees the
/// same ring (the executor records, the Services panel polls `seq`/`recent`).
#[derive(Clone)]
pub struct HttpHistory {
    inner: Arc<Mutex<Inner>>,
}

impl Default for HttpHistory {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                calls: std::collections::VecDeque::new(),
            })),
        }
    }
}

impl HttpHistory {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Append a call, dropping the oldest past the cap.
    pub fn record(&self, call: HttpCall) {
        let mut inner = self.lock();
        inner.calls.push_back(call);
        while inner.calls.len() > HISTORY_CAP {
            inner.calls.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.lock().calls.len()
    }

    /// The `n` most recent calls, newest first.
    pub fn recent(&self, n: usize) -> Vec<HttpCall> {
        self.lock().calls.iter().rev().take(n).cloned().collect()
    }
}

/// One named environment in the manifest: its variables + the hosts it may reach.
///
/// Shared by the HTTP **and** gRPC tools — one place for `base_url` / tokens / the host
/// scope, so widening access is a single deliberate edit rather than two. The gRPC-only
/// fields are inert for HTTP.
#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpEnv {
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// Hosts this environment may reach (exact or dot-suffix). Empty = loopback/private
    /// only (the safe default).
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// **gRPC:** `.proto` files to compile when a server has reflection disabled. Empty
    /// (the default) means the gRPC tool relies on server reflection alone.
    #[serde(default)]
    pub proto_files: Vec<String>,
    /// **gRPC:** import roots for `proto_files`. Empty defaults to each file's own
    /// directory, so a single self-contained `.proto` needs no configuration.
    #[serde(default)]
    pub proto_includes: Vec<String>,
}

/// `.moonlight/http/environments.json`: the active environment name + the set.
#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpManifest {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub environments: BTreeMap<String, HttpEnv>,
}

impl HttpManifest {
    /// The active environment (by `active` name; else the first; else an empty default
    /// whose empty `allowed_hosts` confines requests to loopback/private).
    pub fn active_env(&self) -> HttpEnv {
        self.environments
            .get(&self.active)
            .or_else(|| self.environments.values().next())
            .cloned()
            .unwrap_or_default()
    }

    /// The active environment's display name (for the Services summary).
    pub fn active_name(&self) -> Option<String> {
        if self.environments.contains_key(&self.active) {
            Some(self.active.clone())
        } else {
            self.environments.keys().next().cloned()
        }
    }
}

/// The `.moonlight/http` directory under a project root (created on save).
fn http_dir(root: &Path) -> std::path::PathBuf {
    root.join(".moonlight").join("http")
}

/// Load the HTTP manifest under `root` (`.moonlight/http/environments.json`). A missing
/// or unparseable file yields the empty default (loopback/private-only).
pub fn load_manifest(root: &Path) -> HttpManifest {
    std::fs::read_to_string(http_dir(root).join("environments.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the manifest (e.g. after the operator switches the active environment).
pub fn save_manifest(root: &Path, manifest: &HttpManifest) -> std::io::Result<()> {
    let dir = http_dir(root);
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(manifest).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("environments.json"), text)
}

/// A request the operator saved for reuse (the panel's left list / a collection).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRequest {
    pub name: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
}

/// `.moonlight/http/requests.json`: the operator's saved requests.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HttpRequests {
    #[serde(default)]
    pub requests: Vec<SavedRequest>,
}

/// Load the saved requests under `root` (missing/bad file → empty).
pub fn load_requests(root: &Path) -> HttpRequests {
    std::fs::read_to_string(http_dir(root).join("requests.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the saved requests. Upsert by name lives in the panel; this just writes.
pub fn save_requests(root: &Path, reqs: &HttpRequests) -> std::io::Result<()> {
    let dir = http_dir(root);
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(reqs).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("requests.json"), text)
}

/// Replace every `{{key}}` occurrence in `template` with its variable value (keys with
/// no variable are left intact, so a missing var is visible in the request).
pub fn interpolate(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Encode key/value pairs as an `application/x-www-form-urlencoded` body (spaces → `+`,
/// reserved bytes percent-encoded).
pub fn form_urlencode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Build a JSON **object** from key/value pairs (the JSON body's table mode). Each
/// value is parsed as JSON when it is valid (so `42`, `true`, `{"a":1}`, `[1,2]` stay
/// typed) and falls back to a JSON string otherwise. Pretty-printed.
pub fn json_object_from_pairs(pairs: &[(String, String)]) -> String {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        let val = serde_json::from_str::<serde_json::Value>(v.trim())
            .unwrap_or_else(|_| serde_json::Value::String(v.clone()));
        map.insert(k.clone(), val);
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default()
}

fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The host portion of a URL (no scheme, no port, no path). Empty when unparseable.
fn host_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Strip userinfo and port.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host.split(':').next().unwrap_or(host).to_ascii_lowercase()
}

/// Whether `host` is loopback or a private/link-local address (the default-allow set
/// when an environment lists no `allowed_hosts`).
fn is_local(host: &str) -> bool {
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == "0.0.0.0"
        || host.ends_with(".local")
        || host.ends_with(".localhost")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.")
        || (host.starts_with("172.")
            && host
                .split('.')
                .nth(1)
                .and_then(|o| o.parse::<u8>().ok())
                .is_some_and(|o| (16..=31).contains(&o)))
}

/// Whether a request to `url` is permitted by `allowed_hosts`. Non-empty list → the
/// host must equal or be a dot-suffix of an entry. Empty list → loopback/private only.
pub fn host_allowed(url: &str, allowed_hosts: &[String]) -> bool {
    let host = host_of(url);
    if host.is_empty() {
        return false;
    }
    if allowed_hosts.is_empty() {
        return is_local(&host);
    }
    allowed_hosts.iter().any(|a| {
        let a = a.trim().to_ascii_lowercase();
        host == a || host.ends_with(&format!(".{a}"))
    })
}

/// A request to send: method + URL (post-interpolation) + headers + optional body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestSpec {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// The JSON payload shape the `http_request` verb accepts.
#[derive(Deserialize, Default)]
struct PayloadJson {
    #[serde(default)]
    method: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

/// Parse the verb payload into a [`RequestSpec`]. Accepts a JSON object
/// (`{"method","url","headers","body"}`) or, as a convenience, a bare URL (→ `GET`).
/// Variables are **not** interpolated here — the executor does that with the active env.
pub fn parse_payload(payload: &str) -> Result<RequestSpec, String> {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Err("empty http_request payload".into());
    }
    if trimmed.starts_with('{') {
        let p: PayloadJson =
            serde_json::from_str(trimmed).map_err(|e| format!("invalid JSON payload: {e}"))?;
        if p.url.trim().is_empty() {
            return Err("http_request payload has no url".into());
        }
        let method = if p.method.trim().is_empty() {
            "GET".to_string()
        } else {
            p.method.trim().to_ascii_uppercase()
        };
        Ok(RequestSpec {
            method,
            url: p.url,
            headers: p.headers.into_iter().collect(),
            body: p.body,
        })
    } else {
        // Bare URL convenience form.
        Ok(RequestSpec {
            method: "GET".into(),
            url: trimmed.to_string(),
            headers: Vec::new(),
            body: None,
        })
    }
}

/// The outcome of a blocking send. The executor turns it into an [`HttpCall`] + the
/// agent-facing compact summary; the HTTP panel renders the fuller fields (status text,
/// response headers, the larger body).
pub struct HttpOutcome {
    pub status: Option<u16>,
    /// The reason phrase (e.g. `OK`, `Not Found`); empty on a transport error.
    pub status_text: String,
    pub ms: u64,
    pub ok: bool,
    pub bytes: usize,
    pub error: Option<String>,
    /// Response headers (panel only).
    pub headers: Vec<(String, String)>,
    /// Compact body for the agent / history record (≤ [`BODY_PREVIEW`]).
    pub body_preview: String,
    /// Fuller body for the panel's response pane (≤ [`PANEL_BODY`]).
    pub body: String,
}

/// Perform `spec` synchronously with ureq (call on `spawn_blocking`). Never panics;
/// transport errors surface as `status: None` + `error`.
pub fn send(spec: &RequestSpec) -> HttpOutcome {
    let agent = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .redirects(5)
        .build();
    let mut req = agent.request(&spec.method, &spec.url);
    for (k, v) in &spec.headers {
        req = req.set(k, v);
    }
    let started = Instant::now();
    let result = match &spec.body {
        Some(body) => req.send_string(body),
        None => req.call(),
    };
    let ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(resp) => outcome_from_response(resp, ms, true),
        // A non-2xx/3xx status is an `Err(Status(..))` in ureq, but it IS a response.
        Err(ureq::Error::Status(_, resp)) => outcome_from_response(resp, ms, false),
        Err(ureq::Error::Transport(t)) => HttpOutcome {
            status: None,
            status_text: String::new(),
            ms,
            ok: false,
            bytes: 0,
            error: Some(t.to_string()),
            headers: Vec::new(),
            body_preview: String::new(),
            body: String::new(),
        },
    }
}

fn outcome_from_response(resp: ureq::Response, ms: u64, ok2xx: bool) -> HttpOutcome {
    let status = resp.status();
    let status_text = resp.status_text().to_string();
    // Capture response headers before `into_string` consumes the response.
    let headers: Vec<(String, String)> = resp
        .headers_names()
        .into_iter()
        .filter_map(|n| resp.header(&n).map(|v| (n.clone(), v.to_string())))
        .collect();
    let body = resp.into_string().unwrap_or_default();
    HttpOutcome {
        status: Some(status),
        status_text,
        ms,
        ok: ok2xx && (200..400).contains(&status),
        bytes: body.len(),
        error: None,
        headers,
        body_preview: clip(&body, BODY_PREVIEW),
        body: clip(&body, PANEL_BODY),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn interpolate_replaces_known_vars_and_leaves_unknowns() {
        let v = vars(&[("base_url", "http://localhost:8080"), ("id", "42")]);
        assert_eq!(
            interpolate("{{base_url}}/users/{{id}}", &v),
            "http://localhost:8080/users/42"
        );
        // Unknown var stays literal so it's visible in the request.
        assert_eq!(
            interpolate("{{base_url}}/{{missing}}", &v),
            "http://localhost:8080/{{missing}}"
        );
    }

    #[test]
    fn host_of_extracts_authority() {
        assert_eq!(host_of("http://localhost:8080/x?y=1"), "localhost");
        assert_eq!(
            host_of("https://user:pw@Api.Example.com/v1"),
            "api.example.com"
        );
        assert_eq!(host_of("http://10.0.0.5"), "10.0.0.5");
    }

    #[test]
    fn host_allowed_defaults_to_local_only() {
        // Empty allowlist → loopback/private only.
        assert!(host_allowed("http://localhost:8080/x", &[]));
        assert!(host_allowed("http://127.0.0.1/x", &[]));
        assert!(host_allowed("http://192.168.1.10/x", &[]));
        assert!(host_allowed("http://172.16.0.1/x", &[]));
        assert!(!host_allowed("http://172.32.0.1/x", &[])); // outside 16..=31
        assert!(!host_allowed("https://api.stripe.com/x", &[]));
        assert!(!host_allowed("not a url", &[]));
    }

    #[test]
    fn host_allowed_honours_an_explicit_allowlist() {
        let allow = vec!["example.com".to_string()];
        assert!(host_allowed("https://example.com/x", &allow));
        assert!(host_allowed("https://api.example.com/x", &allow)); // dot-suffix
        assert!(!host_allowed("https://evil-example.com/x", &allow)); // not a dot-suffix
        assert!(!host_allowed("https://stripe.com/x", &allow));
    }

    #[test]
    fn parse_payload_accepts_json_and_bare_url() {
        let j = parse_payload(
            r#"{"method":"post","url":"{{base}}/login","headers":{"X-A":"1"},"body":"hi"}"#,
        )
        .unwrap();
        assert_eq!(j.method, "POST");
        assert_eq!(j.url, "{{base}}/login");
        assert_eq!(j.headers, vec![("X-A".to_string(), "1".to_string())]);
        assert_eq!(j.body.as_deref(), Some("hi"));

        let bare = parse_payload("  http://localhost/health  ").unwrap();
        assert_eq!(bare.method, "GET");
        assert_eq!(bare.url, "http://localhost/health");

        assert!(parse_payload("").is_err());
        assert!(parse_payload(r#"{"method":"GET"}"#).is_err()); // no url
    }

    #[test]
    fn manifest_active_env_falls_back_to_first_then_empty() {
        let mut envs = BTreeMap::new();
        envs.insert(
            "local".to_string(),
            HttpEnv {
                vars: vars(&[("base", "http://localhost")]),
                ..Default::default()
            },
        );
        let m = HttpManifest {
            active: "nope".into(),
            environments: envs,
        };
        // Active name missing → first env.
        assert_eq!(m.active_name().as_deref(), Some("local"));
        assert_eq!(m.active_env().vars.get("base").unwrap(), "http://localhost");
        // Wholly empty manifest → empty env (local-only).
        assert_eq!(HttpManifest::default().active_env(), HttpEnv::default());
        assert_eq!(HttpManifest::default().active_name(), None);
    }

    #[test]
    fn history_rings_and_reports_recent_newest_first() {
        let h = HttpHistory::new();
        assert_eq!(h.len(), 0);
        for i in 0..(HISTORY_CAP + 5) {
            h.record(HttpCall {
                method: "GET".into(),
                url: format!("http://localhost/{i}"),
                status: Some(200),
                ms: 1,
                ok: true,
                bytes: 0,
                at_millis: i as i64,
                error: None,
            });
        }
        assert_eq!(h.len(), HISTORY_CAP); // capped
        let recent = h.recent(3);
        // Newest first; the oldest 5 were dropped.
        assert_eq!(
            recent[0].url,
            format!("http://localhost/{}", HISTORY_CAP + 4)
        );
        assert_eq!(
            recent[2].url,
            format!("http://localhost/{}", HISTORY_CAP + 2)
        );
    }

    #[test]
    fn json_object_keeps_types_and_falls_back_to_string() {
        let pairs = vec![
            ("n".to_string(), "42".to_string()),
            ("b".to_string(), "true".to_string()),
            ("s".to_string(), "hi there".to_string()),
            ("o".to_string(), "{\"a\":1}".to_string()),
        ];
        let json = json_object_from_pairs(&pairs);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["n"], serde_json::json!(42));
        assert_eq!(v["b"], serde_json::json!(true));
        assert_eq!(v["s"], serde_json::json!("hi there"));
        assert_eq!(v["o"], serde_json::json!({"a": 1}));
    }

    #[test]
    fn form_urlencode_encodes_reserved_and_spaces() {
        let pairs = vec![
            ("name".to_string(), "a b".to_string()),
            ("q".to_string(), "x&y=z".to_string()),
        ];
        assert_eq!(form_urlencode(&pairs), "name=a+b&q=x%26y%3Dz");
    }

    #[test]
    fn saved_requests_and_manifest_round_trip_on_disk() {
        let root = std::env::temp_dir().join(format!("ml-http-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // Missing files → empty defaults.
        assert!(load_requests(&root).requests.is_empty());
        assert_eq!(HttpManifest::active_name(&load_manifest(&root)), None);

        // Save + reload requests.
        let reqs = HttpRequests {
            requests: vec![SavedRequest {
                name: "health".into(),
                method: "GET".into(),
                url: "{{base_url}}/health".into(),
                headers: vec![("X-A".into(), "1".into())],
                body: None,
            }],
        };
        save_requests(&root, &reqs).unwrap();
        let back = load_requests(&root);
        assert_eq!(back.requests.len(), 1);
        assert_eq!(back.requests[0].name, "health");
        assert_eq!(back.requests[0].url, "{{base_url}}/health");
        assert_eq!(back.requests[0].headers, vec![("X-A".into(), "1".into())]);

        // Save + reload manifest (active env persists).
        let mut envs = BTreeMap::new();
        envs.insert("local".to_string(), HttpEnv::default());
        let manifest = HttpManifest {
            active: "local".into(),
            environments: envs,
        };
        save_manifest(&root, &manifest).unwrap();
        let m = load_manifest(&root);
        assert_eq!(m.active_name().as_deref(), Some("local"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
