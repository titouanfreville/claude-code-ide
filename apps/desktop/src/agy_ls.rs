//! Antigravity (`agy`) usage client — AGY's own source of token / context / cost data.
//!
//! Claude publishes usage as statusline JSON on disk (see [`crate::obs`]); AGY does
//! **not** — per-conversation tokens live only behind a running `agy` process's own
//! RPC. Each live `agy` session hosts a Connect-protocol server (over HTTP/1.1) on a
//! dynamic **plaintext** localhost port, **no auth required**. Contract, verified live:
//!
//! ```text
//! POST http://127.0.0.1:<port>/exa.language_server_pb.LanguageServerService/<Method>
//! Content-Type: application/json
//! Connect-Protocol-Version: 1
//! <json body>
//! ```
//!
//! - `GetAllCascadeTrajectories` `{}` → `{"trajectorySummaries": { "<conversationId>": {…} }}`
//!   (keyed by our conversation id — the Phase-1 persisted `conversation_id`).
//! - `GetCascadeTrajectoryGeneratorMetadata` `{"cascadeId":"<conversationId>"}` →
//!   `{"generatorMetadata":[ { chatModel:{ usage:{inputTokens,outputTokens,
//!   thinkingOutputTokens}, chatStartMetadata:{contextWindowMetadata:{estimatedTokensUsed}} },
//!   modelDisplayName, responseModel }, … ]}` — one entry per model invocation.
//!
//! Int64 fields are JSON strings (proto3 JSON mapping), so parse tolerantly.
//!
//! **Constraint (verified).** This data is **live-only** — served by the `agy` process
//! while the session runs. On-disk transcripts carry no token counts, the `.db` is
//! opaque protobuf, and the IDE "hub" language server can't resolve CLI conversations.
//! So callers query while live and **cache the last value per conversation id** for
//! after-the-fact display (we cache ourselves — no third-party/tokscale dependency).

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The Connect service path segment.
const SVC: &str = "exa.language_server_pb.LanguageServerService";
/// Per-call timeout — the server is local, so responses are immediate; keep it short so
/// a dead/slow endpoint never stalls the obs poll.
const TIMEOUT: Duration = Duration::from_millis(1500);

/// A reachable `agy` RPC endpoint (plaintext HTTP, localhost, no auth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyEndpoint {
    /// e.g. `http://127.0.0.1:62561`.
    pub base_url: String,
    /// The `agy` process id (for logging / dedup).
    pub pid: u32,
}

/// Aggregated usage for one AGY conversation, summed across its model invocations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgyUsage {
    /// Human model label (e.g. `Gemini 3.1 Pro (High)`), best-effort from the metadata.
    pub model: String,
    /// Cumulative prompt tokens across the session (includes re-sent context per turn).
    pub input_tokens: u64,
    /// Cumulative output tokens (includes `thinking_tokens`).
    pub output_tokens: u64,
    /// Cumulative "thinking" output tokens (subset of `output_tokens`).
    pub thinking_tokens: u64,
    /// Current context-window occupancy (latest `estimatedTokensUsed`).
    pub context_tokens: u64,
}

impl AgyUsage {
    /// `input + output` — total tokens consumed this session.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

// ---- pure parsing (unit-tested, IO-free) ---------------------------------

/// From `ps -ww -eo pid,args` output, the pids of live `agy` CLI processes. Matches a
/// line whose command token (first arg after the pid) has basename `agy`, so a path
/// merely *containing* "agy" isn't picked up.
fn parse_ps_agy_pids(ps: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for line in ps.lines() {
        let mut it = line.trim_start().split_whitespace();
        let Some(pid) = it.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        let Some(cmd) = it.next() else { continue };
        // basename == "agy" (handles `agy`, `/usr/local/bin/agy`, `~/.local/bin/agy`).
        if cmd.rsplit('/').next() == Some("agy") && !out.contains(&pid) {
            out.push(pid);
        }
    }
    out
}

/// From `lsof -nP -iTCP -sTCP:LISTEN -a -p <pid>` output, the distinct localhost LISTEN
/// ports (an `agy` process binds two — the plaintext one and an HTTPS one).
fn parse_lsof_ports(lsof: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for line in lsof.lines() {
        if !line.contains("(LISTEN)") {
            continue;
        }
        let Some(after) = line.split("127.0.0.1:").nth(1) else {
            continue;
        };
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(port) = digits.parse::<u16>() {
            if !out.contains(&port) {
                out.push(port);
            }
        }
    }
    out
}

/// A tolerant int64 read: the Connect JSON encoding serializes int64 as a **string**
/// (proto3 JSON mapping), but numbers appear too — accept both.
fn as_u64(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        .unwrap_or(0)
}

/// Aggregate a `GetCascadeTrajectoryGeneratorMetadata` response into an [`AgyUsage`]:
/// sum `inputTokens`/`outputTokens`/`thinkingOutputTokens` across every entry's
/// `chatModel.usage`, take the latest non-zero `estimatedTokensUsed` as the context
/// size, and the last present `modelDisplayName` (else `responseModel`) as the label.
/// Pure, so unit-tested against the real response shape.
fn aggregate_generator_metadata(resp: &Value) -> Option<AgyUsage> {
    let entries = resp.get("generatorMetadata")?.as_array()?;
    if entries.is_empty() {
        return None;
    }
    let mut u = AgyUsage::default();
    for e in entries {
        let chat = e.get("chatModel");
        if let Some(usage) = chat.and_then(|c| c.get("usage")) {
            u.input_tokens += as_u64(usage.get("inputTokens").unwrap_or(&Value::Null));
            u.output_tokens += as_u64(usage.get("outputTokens").unwrap_or(&Value::Null));
            u.thinking_tokens += as_u64(usage.get("thinkingOutputTokens").unwrap_or(&Value::Null));
        }
        if let Some(ctx) = chat
            .and_then(|c| c.get("chatStartMetadata"))
            .and_then(|m| m.get("contextWindowMetadata"))
            .and_then(|c| c.get("estimatedTokensUsed"))
        {
            let n = as_u64(ctx);
            if n > 0 {
                u.context_tokens = n; // latest wins (entries are chronological)
            }
        }
        // The model label lives under `chatModel` (alongside `usage`): prefer the
        // human `modelDisplayName` (e.g. "Gemini 3.1 Pro (High)"), else the raw
        // `responseModel` (e.g. "gemini-3.1-pro-preview").
        if let Some(m) = chat
            .and_then(|c| c.get("modelDisplayName"))
            .and_then(Value::as_str)
        {
            u.model = m.to_string();
        } else if u.model.is_empty() {
            if let Some(m) = chat
                .and_then(|c| c.get("responseModel"))
                .and_then(Value::as_str)
            {
                u.model = m.to_string();
            }
        }
    }
    Some(u)
}

/// The conversation ids a `GetAllCascadeTrajectories` response advertises (the keys of
/// `trajectorySummaries`).
fn served_ids(resp: &Value) -> Vec<String> {
    resp.get("trajectorySummaries")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

// ---- IO: discovery + Connect calls ---------------------------------------

/// Run a command, returning stdout as a lossy string (empty on failure) — discovery
/// tolerates a missing `ps`/`lsof` by yielding no endpoints.
fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// One Connect unary call. Returns the decoded JSON body, or `None` on any transport /
/// non-2xx / decode error (all "no data from this endpoint"). No auth header — the
/// `agy` server trusts localhost.
fn connect_call(base_url: &str, method: &str, body: &Value) -> Option<Value> {
    let url = format!("{base_url}/{SVC}/{method}");
    ureq::post(&url)
        .timeout(TIMEOUT)
        .set("Content-Type", "application/json")
        .set("Connect-Protocol-Version", "1")
        .send_string(&body.to_string())
        .ok()
        .and_then(|resp| resp.into_string().ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
}

/// Discover every live `agy` RPC endpoint: scan for `agy` processes, `lsof` each pid for
/// its LISTEN ports, and keep the first port that answers `GetAllCascadeTrajectories`
/// over plaintext HTTP. Best-effort — empty when no `agy` session is running.
pub fn discover_endpoints() -> Vec<AgyEndpoint> {
    let ps = run("ps", &["-ww", "-eo", "pid,args"]);
    let mut out = Vec::new();
    for pid in parse_ps_agy_pids(&ps) {
        let lsof = run(
            "lsof",
            &["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pid.to_string()],
        );
        for port in parse_lsof_ports(&lsof) {
            let base_url = format!("http://127.0.0.1:{port}");
            if connect_call(&base_url, "GetAllCascadeTrajectories", &json!({})).is_some() {
                out.push(AgyEndpoint { base_url, pid });
                break; // the plaintext port; the other is HTTPS
            }
        }
    }
    out
}

/// Aggregated usage for `conversation_id`, if a live `agy` endpoint currently serves it.
/// Finds the endpoint advertising the id, then aggregates its generator metadata.
/// `None` when no live session holds the conversation (caller falls back to its cache).
pub fn usage_for_conversation(conversation_id: &str) -> Option<AgyUsage> {
    for ep in discover_endpoints() {
        let list = connect_call(&ep.base_url, "GetAllCascadeTrajectories", &json!({}))?;
        if !served_ids(&list).iter().any(|id| id == conversation_id) {
            continue;
        }
        let meta = connect_call(
            &ep.base_url,
            "GetCascadeTrajectoryGeneratorMetadata",
            &json!({ "cascadeId": conversation_id }),
        )?;
        if let Some(usage) = aggregate_generator_metadata(&meta) {
            return Some(usage);
        }
    }
    None
}

// ---- self-cache (live-only data survives session end / restart) ----------
//
// The RPC serves usage only while the `agy` session is live. We write each successful
// read through to our OWN cache (a small JSON sidecar keyed by conversation id) and read
// it back when no live endpoint is available — so the bar keeps showing a session's last
// tokens after it ends. This is our cache of AGY's own data, not a third-party one.

/// `~/.moonlight-local/agy-usage-cache.json` (gitignored, alongside the DB DSN store).
fn cache_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".moonlight-local")
            .join("agy-usage-cache.json"),
    )
}

/// Load the cache as a `cid -> AgyUsage` map (empty on any read/parse error).
fn cache_load() -> std::collections::HashMap<String, AgyUsage> {
    cache_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Write `usage` for `cid` through to the cache (best-effort; a failure is ignored).
fn cache_put(cid: &str, usage: &AgyUsage) {
    let Some(path) = cache_path() else { return };
    let mut map = cache_load();
    map.insert(cid.to_string(), usage.clone());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, text);
    }
}

/// The last cached usage for `conversation_id`, if any (cheap: a single file read, no
/// process scan / RPC). Used on the fast poll path between live refreshes.
pub fn cached_usage(conversation_id: &str) -> Option<AgyUsage> {
    cache_load().get(conversation_id).cloned()
}

/// Aggregated usage for `conversation_id`, preferring a **live** `agy` endpoint (which we
/// then cache) and falling back to the last cached value when no session currently serves
/// it. `None` only when it is neither live nor ever cached.
pub fn usage_for_conversation_cached(conversation_id: &str) -> Option<AgyUsage> {
    if let Some(usage) = usage_for_conversation(conversation_id) {
        cache_put(conversation_id, &usage);
        return Some(usage);
    }
    cache_load().get(conversation_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agy_pids_by_command_basename() {
        let ps = "  96751 agy --conversation 59d9d44e-c547-47c7-a04f-b7e3ebb15ac6\n\
                   97864 /Users/x/.local/bin/agy --conversation 544c7023\n\
                    2022 /Applications/Antigravity.app/Contents/Resources/bin/language_server --standalone\n\
                    3000 node /some/agystuff/server.js";
        // Only the two real `agy` commands; language_server and the `agystuff` path are skipped.
        assert_eq!(parse_ps_agy_pids(ps), vec![96751, 97864]);
    }

    #[test]
    fn parses_listen_ports_from_lsof_deduped() {
        let lsof = "COMMAND  PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n\
                     agy 96751 t 6u IPv4 0x1 0t0 TCP 127.0.0.1:62560 (LISTEN)\n\
                     agy 96751 t 7u IPv4 0x2 0t0 TCP 127.0.0.1:62561 (LISTEN)\n\
                     agy 96751 t 8u IPv4 0x3 0t0 TCP 127.0.0.1:62561 (LISTEN)\n\
                     agy 96751 t 9u IPv4 0x4 0t0 TCP 127.0.0.1:9000 (ESTABLISHED)";
        assert_eq!(parse_lsof_ports(lsof), vec![62560, 62561]);
    }

    #[test]
    fn served_ids_reads_trajectory_summary_keys() {
        let resp = json!({ "trajectorySummaries": { "cid-a": {}, "cid-b": {} } });
        let mut ids = served_ids(&resp);
        ids.sort();
        assert_eq!(ids, vec!["cid-a".to_string(), "cid-b".to_string()]);
        assert!(served_ids(&json!({})).is_empty());
    }

    #[test]
    fn aggregates_tokens_context_and_model_from_real_shape() {
        // Mirrors the live `GetCascadeTrajectoryGeneratorMetadata` response: int64 as
        // strings, output = thinking + response, context in chatStartMetadata, and a
        // modelDisplayName that only some entries carry.
        let resp = json!({
            "generatorMetadata": [
                {
                    "chatModel": {
                        "usage": {
                            "inputTokens": "27850",
                            "outputTokens": "904",
                            "thinkingOutputTokens": "811"
                        },
                        "chatStartMetadata": {
                            "contextWindowMetadata": { "estimatedTokensUsed": "1358" }
                        },
                        "modelDisplayName": "Gemini 3.1 Pro (High)"
                    }
                },
                {
                    "chatModel": {
                        "usage": {
                            "inputTokens": 100,
                            "outputTokens": 50,
                            "thinkingOutputTokens": 10
                        },
                        "chatStartMetadata": {
                            "contextWindowMetadata": { "estimatedTokensUsed": "124446" }
                        },
                        "responseModel": "gemini-3.1-pro-preview"
                    }
                }
            ]
        });
        let u = aggregate_generator_metadata(&resp).expect("usage");
        assert_eq!(u.input_tokens, 27_950);
        assert_eq!(u.output_tokens, 954);
        assert_eq!(u.thinking_tokens, 821);
        assert_eq!(u.total_tokens(), 28_904);
        assert_eq!(u.context_tokens, 124_446, "latest estimatedTokensUsed wins");
        assert_eq!(
            u.model, "Gemini 3.1 Pro (High)",
            "display name kept over responseModel"
        );
    }

    #[test]
    #[ignore = "live: requires a running `agy` session; run with --ignored"]
    fn live_end_to_end_usage() {
        // A self-discovering smoke test against a live `agy` session: find an endpoint,
        // take a conversation it serves, and confirm the full pipeline yields real tokens.
        let endpoints = discover_endpoints();
        eprintln!("discovered endpoints: {endpoints:?}");
        assert!(!endpoints.is_empty(), "no live agy endpoint found");
        let cid = endpoints
            .iter()
            .filter_map(|ep| connect_call(&ep.base_url, "GetAllCascadeTrajectories", &json!({})))
            .flat_map(|resp| served_ids(&resp))
            .next()
            .expect("a live endpoint serving a conversation");
        let u = usage_for_conversation(&cid).expect("usage for live conversation");
        eprintln!(
            "cid={cid} model={} input={} output={} thinking={} total={} ctx={}",
            u.model,
            u.input_tokens,
            u.output_tokens,
            u.thinking_tokens,
            u.total_tokens(),
            u.context_tokens
        );
        assert!(u.total_tokens() > 0);
        assert!(!u.model.is_empty());
    }

    #[test]
    fn aggregate_none_on_empty_metadata() {
        assert!(aggregate_generator_metadata(&json!({ "generatorMetadata": [] })).is_none());
        assert!(aggregate_generator_metadata(&json!({})).is_none());
    }
}
