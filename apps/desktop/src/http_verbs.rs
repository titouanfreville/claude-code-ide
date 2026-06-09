//! The [`VerbExecutor`] behind the MCP **`http_request`** verb — the agent's gated
//! outbound HTTP. Wraps the rest of the executor stack and handles only `HttpRequest`,
//! delegating the rest. Policy/audit live a layer up in `ActorService` + the PDP
//! (`HttpRequest` is `Risky`, so frozen phases deny it and the approval gate applies);
//! this layer does the work:
//!
//! 1. parse the payload (`{method,url,headers,body}` or a bare URL),
//! 2. load the session's HTTP manifest (`.moonlight/http/environments.json`) and
//!    interpolate `{{vars}}` from the active environment,
//! 3. enforce the environment's host-scope (the SSRF gate — loopback/private only
//!    unless `allowed_hosts` widens it),
//! 4. send it on a blocking task ([`http::send`] via `spawn_blocking`),
//! 5. record the call in the shared [`HttpHistory`] (the Services summary reads it)
//!    and return a compact `status · ms · bytes` summary + body preview to the agent.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use moonlight_domain::errors::ControlError;
use moonlight_domain::ids::SessionId;
use moonlight_domain::ports::mcp::VerbExecutor;
use moonlight_domain::trust::McpVerb;

use crate::http::{self, HttpCall, HttpHistory};

pub struct HttpVerbExecutor {
    history: HttpHistory,
    inner: Arc<dyn VerbExecutor>,
}

impl HttpVerbExecutor {
    pub fn new(history: HttpHistory, inner: Arc<dyn VerbExecutor>) -> Self {
        Self { history, inner }
    }

    /// Wall-clock millis now (history timestamps; mirrors the panel/workspace helper).
    fn now_millis() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    async fn http_request(
        &self,
        root: Option<&Path>,
        payload: &str,
    ) -> Result<String, ControlError> {
        let spec0 = http::parse_payload(payload).map_err(ControlError::Unsupported)?;

        // Resolve variables + host-scope against the session's active environment.
        let manifest = root.map(http::load_manifest).unwrap_or_default();
        let env = manifest.active_env();
        let url = http::interpolate(&spec0.url, &env.vars);
        if !http::host_allowed(&url, &env.allowed_hosts) {
            return Err(ControlError::Unsupported(format!(
                "host not allowed by the active HTTP environment: {url} \
                 (add it to allowed_hosts in .moonlight/http/environments.json)"
            )));
        }
        let headers = spec0
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), http::interpolate(v, &env.vars)))
            .collect();
        let spec = http::RequestSpec {
            method: spec0.method,
            url: url.clone(),
            headers,
            body: spec0.body.map(|b| http::interpolate(&b, &env.vars)),
        };

        // Blocking ureq off the async runtime.
        let method = spec.method.clone();
        let outcome = tokio::task::spawn_blocking(move || http::send(&spec))
            .await
            .map_err(|e| ControlError::Transport(format!("http task: {e}")))?;

        self.history.record(HttpCall {
            method: method.clone(),
            url: url.clone(),
            status: outcome.status,
            ms: outcome.ms,
            ok: outcome.ok,
            bytes: outcome.bytes,
            at_millis: Self::now_millis(),
            error: outcome.error.clone(),
        });

        // Compact agent-facing summary (the actor compacts further if needed).
        let head = match (outcome.status, &outcome.error) {
            (Some(code), _) => format!("{method} {url} → {code} · {}ms · {}B", outcome.ms, outcome.bytes),
            (None, Some(err)) => format!("{method} {url} → error · {}ms · {err}", outcome.ms),
            (None, None) => format!("{method} {url} → (no response) · {}ms", outcome.ms),
        };
        Ok(if outcome.body_preview.is_empty() {
            head
        } else {
            format!("{head}\n{}", outcome.body_preview)
        })
    }
}

#[async_trait]
impl VerbExecutor for HttpVerbExecutor {
    async fn execute(
        &self,
        session: &SessionId,
        root: Option<&Path>,
        verb: McpVerb,
        payload: &str,
    ) -> Result<String, ControlError> {
        match verb {
            McpVerb::HttpRequest => self.http_request(root, payload).await,
            other => self.inner.execute(session, root, other, payload).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inner executor that records delegation (non-http verbs must fall through).
    struct EchoInner;
    #[async_trait]
    impl VerbExecutor for EchoInner {
        async fn execute(
            &self,
            _session: &SessionId,
            _root: Option<&Path>,
            verb: McpVerb,
            _payload: &str,
        ) -> Result<String, ControlError> {
            Ok(format!("inner:{verb:?}"))
        }
    }

    fn exec() -> HttpVerbExecutor {
        HttpVerbExecutor::new(HttpHistory::new(), Arc::new(EchoInner))
    }

    fn run(
        ex: &HttpVerbExecutor,
        verb: McpVerb,
        payload: &str,
    ) -> Result<String, ControlError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ex.execute(&SessionId::new("s1"), None, verb, payload))
    }

    #[test]
    fn non_http_verbs_delegate_to_inner() {
        let out = run(&exec(), McpVerb::RunWithCoverage, "").unwrap();
        assert_eq!(out, "inner:RunWithCoverage");
    }

    #[test]
    fn a_public_host_is_refused_before_any_network_call() {
        // No manifest (root None) → empty env → local-only. A public host is refused
        // by the host-scope gate without recording a call.
        let ex = exec();
        let err = run(&ex, McpVerb::HttpRequest, "https://api.stripe.com/v1/charges").unwrap_err();
        assert!(err.to_string().contains("host not allowed"), "{err}");
        assert_eq!(ex.history.len(), 0, "a refused request must not hit history");
    }

    #[test]
    fn a_malformed_payload_is_refused() {
        let err = run(&exec(), McpVerb::HttpRequest, "{bad json").unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "{err}");
    }
}
