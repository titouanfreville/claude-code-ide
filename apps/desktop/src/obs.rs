//! Claude Code **statusline JSON** ingestion → per-session observability.
//!
//! Claude Code runs a `statusLine` command on every render, piping a JSON snapshot
//! (model, cost, duration, output style, …) on stdin. MoonlightCode registers
//! `moonlight statusline` as that command **only for app-launched sessions** — via
//! the `--settings` file from [`statusline_settings_flag`], which *merges* on top of
//! the user's settings (so their global statusline/hooks, e.g. the OMC HUD, are kept
//! and our gating hook still fires). The subcommand parses the JSON, derives the live
//! context size from the transcript, and drops a small `<support>/obs/<id>.json`. The
//! running app polls that directory ([`views::obs_store`]) and the bottom status bar
//! renders the selected session's metrics.
//!
//! This is **external observation** the UI surfaces directly (like the transcript the
//! session monitor already reads) — it never crosses the engine bus. The statusline
//! payload carries model/cost/duration/output-style/ctx; quotas, skills, and
//! subagents are NOT in it (those remain `—` in the bar until another source lands).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Per-session metrics captured from the statusline JSON (+ transcript-derived ctx).
/// Every field tolerates absence — the payload is untrusted and may evolve.
/// `serde(default)` so snapshots written by an older binary (missing newer fields,
/// e.g. `ctx_limit`) still load instead of silently vanishing from the read-model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionObs {
    /// Model display name, e.g. "Opus 4.8" (`model.display_name`).
    pub model: String,
    /// Wall-clock session duration in ms (`cost.total_duration_ms`).
    pub session_ms: u64,
    /// Cumulative session cost in USD (`cost.total_cost_usd`).
    pub cost_usd: f64,
    pub lines_added: u64,
    pub lines_removed: u64,
    /// Output-style / persona name (`output_style.name`), e.g. "default".
    pub persona: String,
    /// Context tokens currently in the window: the payload's
    /// `context_window.current_usage` prompt side when present, else the latest
    /// transcript `usage` (older CC versions).
    pub ctx_tokens: u64,
    /// The context window size: the payload's `context_window.context_window_size`
    /// when present, else inferred (1M for long-context variants, else 200k).
    pub ctx_limit: u64,
    /// CC's **native** context-usage % (`context_window.used_percentage`), when the
    /// payload carries one — authoritative over any tokens÷window arithmetic (CC
    /// accounts for its own reserves). `None` on older CC versions.
    pub ctx_used_pct: Option<u8>,
    /// CC's own flag that the context exceeds 200k tokens (`exceeds_200k_tokens`).
    pub exceeds_200k: bool,
    /// Cumulative tokens consumed this session (input + output). AGY-only, from its
    /// language-server RPC (see [`crate::agy_ls`]); `0` for Claude (whose statusline
    /// exposes cost, not a running token total). Used for the AGY pay-as-you-go readout.
    pub total_tokens: u64,
    /// When this snapshot was written (epoch ms), for staleness checks.
    pub updated_ms: i64,
}

impl SessionObs {
    /// Context usage as a % of the window: CC's native `used_percentage` when the
    /// payload carried one, else computed from tokens ÷ window. `None` when neither
    /// source has data (the bar then shows `—`). The single source for both the
    /// status-bar ctx gauge and the auto-compact policy.
    pub fn ctx_pct(&self) -> Option<u8> {
        self.ctx_used_pct.or_else(|| {
            (self.ctx_tokens > 0 && self.ctx_limit > 0)
                .then(|| ((self.ctx_tokens * 100 / self.ctx_limit).min(100)) as u8)
        })
    }
}

/// The slice of Claude Code's statusline stdin payload we read. Defensive: missing or
/// renamed keys degrade to defaults rather than failing the whole parse.
#[derive(Debug, Default, Deserialize)]
struct StatuslineInput {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    transcript_path: String,
    #[serde(default)]
    model: ModelIn,
    #[serde(default)]
    cost: CostIn,
    #[serde(default)]
    output_style: OutputStyleIn,
    #[serde(default)]
    context_window: ContextWindowIn,
    #[serde(default)]
    exceeds_200k_tokens: bool,
}

/// CC's native context metrics (newer CC versions). When present these are
/// authoritative: `context_window_size` is the *real* window (a 1M session is not
/// always marked in the model name) and `used_percentage` is CC's own figure.
#[derive(Debug, Default, Deserialize)]
struct ContextWindowIn {
    #[serde(default)]
    context_window_size: Option<u64>,
    #[serde(default)]
    total_input_tokens: Option<u64>,
    #[serde(default)]
    used_percentage: Option<f64>,
    #[serde(default)]
    current_usage: Option<CurrentUsageIn>,
}

#[derive(Debug, Default, Deserialize)]
struct CurrentUsageIn {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

impl ContextWindowIn {
    /// Prompt-side tokens currently in the window, when the payload reports them:
    /// the `current_usage` sum, else `total_input_tokens`. `None`/0 → caller falls
    /// back to the transcript scan.
    fn tokens(&self) -> Option<u64> {
        let usage = self
            .current_usage
            .as_ref()
            .map(|u| u.input_tokens + u.cache_creation_input_tokens + u.cache_read_input_tokens)
            .filter(|&t| t > 0);
        usage.or(self.total_input_tokens.filter(|&t| t > 0))
    }

    /// CC's native usage %, rounded/clamped; `None` when absent or non-positive
    /// (transient snapshots report 0 — treated as "no data", like OMC's HUD does).
    fn used_pct(&self) -> Option<u8> {
        self.used_percentage
            .filter(|p| p.is_finite() && *p > 0.0)
            .map(|p| p.round().clamp(0.0, 100.0) as u8)
    }
}

#[derive(Debug, Default, Deserialize)]
struct ModelIn {
    #[serde(default)]
    id: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Debug, Default, Deserialize)]
struct CostIn {
    #[serde(default)]
    total_duration_ms: u64,
    #[serde(default)]
    total_cost_usd: f64,
    #[serde(default)]
    total_lines_added: u64,
    #[serde(default)]
    total_lines_removed: u64,
}

#[derive(Debug, Default, Deserialize)]
struct OutputStyleIn {
    #[serde(default)]
    name: String,
}

/// Parse a statusline JSON payload into `(session_id, SessionObs)`. `now_ms` stamps
/// the snapshot (passed in so this stays clock-free + unit-testable). Returns `None`
/// when the payload is unparsable or carries no session id.
pub fn ingest(json: &str, now_ms: i64) -> Option<(String, SessionObs)> {
    let input: StatuslineInput = serde_json::from_str(json).ok()?;
    if input.session_id.is_empty() {
        return None;
    }
    // Tokens: the payload's native `context_window` figures when present, else the
    // transcript-tail scan (older CC versions, which lack the block entirely).
    let ctx_tokens = input.context_window.tokens().unwrap_or_else(|| {
        if input.transcript_path.is_empty() {
            0
        } else {
            ctx_tokens_from_transcript(Path::new(&input.transcript_path)).unwrap_or(0)
        }
    });
    // Window: the payload's real `context_window_size` when present, else inferred
    // from the model name / the exceeds-200k flag.
    let ctx_limit = input
        .context_window
        .context_window_size
        .filter(|&s| s > 0)
        .unwrap_or_else(|| {
            context_limit(
                &input.model.id,
                &input.model.display_name,
                input.exceeds_200k_tokens,
            )
        });
    let obs = SessionObs {
        model: input.model.display_name,
        session_ms: input.cost.total_duration_ms,
        cost_usd: input.cost.total_cost_usd,
        lines_added: input.cost.total_lines_added,
        lines_removed: input.cost.total_lines_removed,
        persona: input.output_style.name,
        ctx_tokens,
        ctx_limit,
        ctx_used_pct: input.context_window.used_pct(),
        exceeds_200k: input.exceeds_200k_tokens,
        total_tokens: 0, // Claude's statusline exposes cost, not a running token total.
        updated_ms: now_ms,
    };
    Some((input.session_id, obs))
}

/// Best-effort context size: scan the transcript JSONL from the end for the most
/// recent message `usage`, summing the prompt-side tokens (input + both caches).
/// Bounded to the last 256 KiB so a huge transcript never stalls the per-render
/// statusline. A partial first line (from the mid-file seek) simply fails to parse
/// and is skipped — only the complete trailing lines matter.
fn ctx_tokens_from_transcript(path: &Path) -> Option<u64> {
    let tail = read_tail(path, 256 * 1024)?;
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let usage = v
            .get("message")
            .and_then(|m| m.get("usage"))
            .or_else(|| v.get("usage"));
        if let Some(u) = usage {
            let get = |k: &str| u.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
            let total = get("input_tokens")
                + get("cache_read_input_tokens")
                + get("cache_creation_input_tokens");
            if total > 0 {
                return Some(total);
            }
        }
    }
    None
}

/// Read up to the last `max` bytes of a file (UTF-8 lossy), for a bounded tail scan.
fn read_tail(path: &Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// MoonlightCode's support directory (shared with layout/projects state).
pub(crate) fn support_dir() -> Option<PathBuf> {
    crate::support::support_dir()
}

/// Claude Code's config dir (`$CLAUDE_CONFIG_DIR`, else `~/.claude`).
fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude"))
}

/// A model's context window, used to render ctx as a **% of the window**: 1M for the
/// long-context variants, else the standard 200k. A session is long-context when its
/// id/name carries a `1m` marker **or** CC reports `exceeds_200k_tokens` — the payload
/// doesn't always surface the marker (observed: a >200k session whose model read plain
/// "Opus 4.8"), and only a 1M window can legitimately exceed 200k. Without this the
/// ctx gauge divides by 200k and pegs at 100% even when the real window is ~20% used.
fn context_limit(model_id: &str, model_name: &str, exceeds_200k: bool) -> u64 {
    let s = format!("{model_id} {model_name}").to_ascii_lowercase();
    if exceeds_200k || s.contains("1m") {
        1_000_000
    } else {
        200_000
    }
}

/// Context window for Antigravity's Gemini models — **1,048,576 tokens (1M)**, the
/// window shared by the current Gemini Pro/Flash line (2.5 Pro, 3 Pro). AGY's usage
/// RPC reports occupancy (`estimatedTokensUsed`) but not the window size, so we supply
/// this constant to render the ctx gauge as a percentage (same notation as Claude)
/// rather than a bare token count.
pub(crate) const GEMINI_CONTEXT_WINDOW: u64 = 1_048_576;

/// Account usage quota — the 5h rolling, weekly, and Sonnet-weekly utilization %s
/// (the headline OMC-HUD numbers). `None` for any source we don't have.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Quota {
    pub five_hour_pct: Option<u8>,
    /// When the 5-hour rolling window resets, as Unix epoch **seconds** (UTC). The
    /// status bar turns this into a live countdown; `None` when the source omits it.
    pub five_hour_resets_at: Option<i64>,
    pub weekly_pct: Option<u8>,
    pub sonnet_pct: Option<u8>,
}

/// oh-my-claudecode's usage cache shape (`.usage-cache-anthropic.json`).
#[derive(Debug, Default, Deserialize)]
struct UsageCache {
    #[serde(default)]
    data: UsageData,
    #[serde(default)]
    error: bool,
}

#[derive(Debug, Default, Deserialize)]
struct UsageData {
    #[serde(rename = "fiveHourPercent", default)]
    five_hour: Option<f64>,
    #[serde(rename = "fiveHourResetsAt", default)]
    five_hour_resets_at: Option<String>,
    #[serde(rename = "weeklyPercent", default)]
    weekly: Option<f64>,
    #[serde(rename = "sonnetWeeklyPercent", default)]
    sonnet: Option<f64>,
}

/// Read the account usage quota, **self-fetched** from Anthropic's OAuth usage
/// endpoint so it works without oh-my-claudecode, falling back to OMC's cache, then
/// `None`. This is the source the status bar polls.
pub fn quota() -> Option<Quota> {
    fetch_quota().or_else(load_quota)
}

/// The user's Claude OAuth access token (`claudeAiOauth.accessToken` in
/// `~/.claude/.credentials.json`) — the same token Claude Code itself uses.
fn oauth_access_token() -> Option<String> {
    let path = claude_config_dir()?.join(".credentials.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(str::to_owned)
}

/// Raw `GET /api/oauth/usage` response — each window's `utilization` is a 0–100 %.
#[derive(Debug, Default, Deserialize)]
struct UsageApi {
    #[serde(default)]
    five_hour: Option<UsageWindow>,
    #[serde(default)]
    seven_day: Option<UsageWindow>,
    #[serde(default)]
    seven_day_sonnet: Option<UsageWindow>,
}

#[derive(Debug, Default, Deserialize)]
struct UsageWindow {
    #[serde(default)]
    utilization: Option<f64>,
    /// RFC3339 UTC instant the window resets (e.g. `2026-06-08T12:10:00.735Z`).
    #[serde(default)]
    resets_at: Option<String>,
}

/// Fetch the account usage quota **directly from Anthropic** (`GET
/// https://api.anthropic.com/api/oauth/usage`, `Authorization: Bearer <CC OAuth
/// token>` + `anthropic-beta: oauth-2025-04-20`), so quotas show without OMC. Runs
/// `curl` with its options on **stdin** (`--config -`) so the token never lands in
/// argv/`ps`. `None` on any failure (no token, offline, non-2xx, parse error) — the
/// caller then falls back to OMC's cache, then `—`. Network-bound: call infrequently.
fn fetch_quota() -> Option<Quota> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let token = oauth_access_token()?;
    // curl config (options from stdin): keeps the bearer token out of the process args.
    let config = format!(
        "url = \"https://api.anthropic.com/api/oauth/usage\"\n\
         header = \"Authorization: Bearer {token}\"\n\
         header = \"anthropic-beta: oauth-2025-04-20\"\n\
         silent\nfail\nmax-time = 10\n"
    );
    let mut child = Command::new("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.as_mut()?.write_all(config.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_usage_api(&out.stdout)
}

/// Map the raw usage-API JSON to a [`Quota`]. `None` when no window has a figure.
fn parse_usage_api(bytes: &[u8]) -> Option<Quota> {
    let api: UsageApi = serde_json::from_slice(bytes).ok()?;
    let pct = |w: Option<UsageWindow>| {
        w.and_then(|x| x.utilization)
            .map(|u| u.round().clamp(0.0, 100.0) as u8)
    };
    // Capture the 5h reset before `pct` consumes the window.
    let five_hour_resets_at = api
        .five_hour
        .as_ref()
        .and_then(|w| w.resets_at.as_deref())
        .and_then(parse_rfc3339_utc);
    let q = Quota {
        five_hour_pct: pct(api.five_hour),
        five_hour_resets_at,
        weekly_pct: pct(api.seven_day),
        sonnet_pct: pct(api.seven_day_sonnet),
    };
    (q.five_hour_pct.is_some() || q.weekly_pct.is_some() || q.sonnet_pct.is_some()).then_some(q)
}

/// Read the account usage quota from **oh-my-claudecode's usage cache**
/// (`<config>/plugins/oh-my-claudecode/.usage-cache-anthropic.json`, kept fresh by
/// OMC's daemon) — the fallback when the self-fetch can't (no token / offline).
/// `None` when OMC isn't installed or the cache is absent/errored.
fn load_quota() -> Option<Quota> {
    let path = claude_config_dir()?.join("plugins/oh-my-claudecode/.usage-cache-anthropic.json");
    parse_quota(&std::fs::read_to_string(path).ok()?)
}

/// Parse the usage-cache JSON into a [`Quota`]. `None` when unparsable or the cache
/// reports an `error` (stale figures aren't shown as if fresh).
fn parse_quota(text: &str) -> Option<Quota> {
    let cache: UsageCache = serde_json::from_str(text).ok()?;
    if cache.error {
        return None;
    }
    let pct = |v: Option<f64>| v.map(|p| p.round().clamp(0.0, 100.0) as u8);
    Some(Quota {
        five_hour_pct: pct(cache.data.five_hour),
        five_hour_resets_at: cache
            .data
            .five_hour_resets_at
            .as_deref()
            .and_then(parse_rfc3339_utc),
        weekly_pct: pct(cache.data.weekly),
        sonnet_pct: pct(cache.data.sonnet),
    })
}

/// Directory holding per-session obs snapshots (`<support>/obs/`).
pub fn obs_dir() -> Option<PathBuf> {
    Some(support_dir()?.join("obs"))
}

/// Map a session id to a safe filename stem.
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Atomically write a session's obs snapshot to `<obs_dir>/<id>.json`. Fail-quiet
/// (this runs inside the throttled `moonlight statusline` subcommand).
pub fn write_obs(session_id: &str, obs: &SessionObs) {
    let Some(dir) = obs_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let stem = sanitize(session_id);
    let Ok(json) = serde_json::to_string(obs) else {
        return;
    };
    let tmp = dir.join(format!(".{stem}.tmp"));
    if std::fs::write(&tmp, json.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(format!("{stem}.json")));
    }
}

/// Load every `<obs_dir>/*.json` as `(session_id, SessionObs)` (UI poll).
pub fn load_all() -> Vec<(String, SessionObs)> {
    let Some(dir) = obs_dir() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(obs) = serde_json::from_str::<SessionObs>(&text) {
                out.push((stem.to_string(), obs));
            }
        }
    }
    out
}

/// Resolve each managed **Antigravity** session's observability as `(managed_id,
/// SessionObs)`, from AGY's OWN data (no third-party cache): model + duration from the
/// transcript, and — layered on top — live tokens / context / authoritative model from
/// the `agy` language-server RPC (see [`crate::agy_ls`]).
///
/// Each entry is `(managed_id, root, conversation_id)`. When the launch-time
/// `conversation_id` has been **discovered + persisted** (see
/// [`discover_agy_conversation`]) we key on it **exactly**; otherwise we fall back to a
/// root bridge — the newest AGY conversation whose workspace matches `root` (a display
/// stopgap for the brief window before correlation lands, since a root is reused across
/// conversations). The result is keyed by the **managed id** (what the status bar looks up).
///
/// `fetch_live` gates the (expensive: process scan + HTTP) live usage refresh — the
/// caller sets it on a slow cadence and the fast ticks read the write-through cache.
pub fn agy_models(
    managed: &[(String, String, Option<String>)],
    fetch_live: bool,
) -> Vec<(String, SessionObs)> {
    if managed.is_empty() {
        return Vec::new();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let base = PathBuf::from(&home).join(".gemini").join("antigravity-cli");
    // workspace -> most-recent (timestamp, conversationId), from the CLI's own history —
    // only built (and read) for sessions still lacking a persisted conversation id.
    let mut latest: std::collections::HashMap<String, (i64, String)> =
        std::collections::HashMap::new();
    let need_bridge = managed.iter().any(|(_, _, cid)| cid.is_none());
    if need_bridge {
        if let Ok(text) = std::fs::read_to_string(base.join("history.jsonl")) {
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let (Some(ws), Some(cid)) = (
                    v.get("workspace").and_then(|x| x.as_str()),
                    v.get("conversationId").and_then(|x| x.as_str()),
                ) else {
                    continue;
                };
                let ts = v.get("timestamp").and_then(|x| x.as_i64()).unwrap_or(0);
                let e = latest
                    .entry(ws.to_string())
                    .or_insert((i64::MIN, String::new()));
                if ts >= e.0 {
                    *e = (ts, cid.to_string());
                }
            }
        }
    }
    let mut out = Vec::new();
    for (mid, root, persisted) in managed {
        // Prefer the persisted, launch-correlated conversation id (exact); else bridge by
        // root (exact workspace match, else the newest related conversation).
        let cid = persisted.clone().or_else(|| {
            latest.get(root).map(|(_, c)| c.clone()).or_else(|| {
                latest
                    .iter()
                    .filter(|(ws, _)| {
                        ws.starts_with(root.as_str()) || root.starts_with(ws.as_str())
                    })
                    .max_by_key(|(_, (ts, _))| *ts)
                    .map(|(_, (_, c))| c.clone())
            })
        });
        let Some(cid) = cid else {
            continue;
        };
        let transcript = base
            .join("brain")
            .join(&cid)
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        // Live usage from the `agy` RPC when refreshing this tick, else the last cached
        // value (both AGY's own data — see `agy_ls`). Carries the authoritative model
        // label + cumulative tokens + context occupancy.
        let usage = if fetch_live {
            crate::agy_ls::usage_for_conversation_cached(&cid)
        } else {
            crate::agy_ls::cached_usage(&cid)
        };
        // Model: the RPC's authoritative label wins; else parse it from the transcript.
        let model = usage
            .as_ref()
            .map(|u| u.model.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| read_head_model(&transcript));
        // Emit a row if we learned anything about this session.
        if model.is_none() && usage.is_none() {
            continue;
        }
        let session_ms = read_agy_duration(&transcript).unwrap_or(0);
        out.push((
            mid.clone(),
            SessionObs {
                model: model.unwrap_or_default(),
                session_ms,
                ctx_tokens: usage.as_ref().map(|u| u.context_tokens).unwrap_or(0),
                // Supply Gemini's 1M window so `ctx_pct()` renders the context gauge as a
                // percentage (same as Claude); AGY's RPC reports occupancy but not the size.
                ctx_limit: GEMINI_CONTEXT_WINDOW,
                total_tokens: usage.as_ref().map(|u| u.total_tokens()).unwrap_or(0),
                ..Default::default()
            },
        ));
    }
    out
}

/// Discover the AGY `conversationId` a freshly-launched managed session created, by
/// correlating its `root` + launch time against AGY's own `history.jsonl`. AGY has no
/// `--session-id`, so it mints its own id on first interaction; we identify it as the
/// **earliest conversation created at/after `since_millis`** (the record's creation,
/// ≈ launch) whose workspace equals `root`. Returns `None` until AGY has logged a
/// conversation for this launch (the caller retries on the next poll, then persists).
///
/// Timestamps in `history.jsonl` are epoch **milliseconds** (directly comparable to a
/// [`Timestamp`](moonlight_domain::ids::Timestamp)'s `as_millis`). Limitation: two AGY
/// sessions launched into the *same* root before either interacts are inherently
/// ambiguous (no per-session id to disambiguate) — the earliest-after-launch match is
/// the best available heuristic.
pub fn discover_agy_conversation(root: &str, since_millis: i64) -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let base = PathBuf::from(&home).join(".gemini").join("antigravity-cli");
    let text = std::fs::read_to_string(base.join("history.jsonl")).ok()?;
    earliest_conversation_after(&text, root, since_millis)
}

/// Pure correlation core of [`discover_agy_conversation`] (IO-free, so unit-testable):
/// from `history.jsonl` text, the id of the conversation whose *earliest* line in
/// `root` is the smallest timestamp `>= since_millis`.
fn earliest_conversation_after(history: &str, root: &str, since_millis: i64) -> Option<String> {
    // Per-conversation earliest timestamp (its creation), restricted to this workspace.
    let mut first_seen: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for line in history.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (Some(ws), Some(cid)) = (
            v.get("workspace").and_then(|x| x.as_str()),
            v.get("conversationId").and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        if ws != root {
            continue;
        }
        let ts = v.get("timestamp").and_then(|x| x.as_i64()).unwrap_or(0);
        let e = first_seen.entry(cid.to_string()).or_insert(i64::MAX);
        if ts < *e {
            *e = ts;
        }
    }
    first_seen
        .into_iter()
        .filter(|(_, ts)| *ts >= since_millis)
        .min_by_key(|(_, ts)| *ts)
        .map(|(cid, _)| cid)
}

/// Read a bounded head of an AGY transcript and parse its session model. The model is set
/// in the first `USER_INPUT` record, so a small head read suffices and is cheap to run
/// each poll. `None` if no model marker is present.
fn read_head_model(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 16 * 1024];
    let n = f.read(&mut buf).ok()?;
    parse_agy_model(&String::from_utf8_lossy(&buf[..n]))
}

/// Extract the human-readable model from an AGY `USER_SETTINGS_CHANGE`, e.g.
/// ``…changed setting `Model Selection` from None to Gemini 3.5 Flash (Medium).…`` →
/// `"Gemini 3.5 Flash"`. Untrusted input: returns `None` on any unexpected shape.
fn parse_agy_model(text: &str) -> Option<String> {
    let anchor = text.find("Model Selection")?;
    let rest = &text[anchor..];
    let after_to = &rest[rest.find(" to ")? + " to ".len()..];
    // The name ends at the reasoning-level " (" or the sentence ". "; cap the length so a
    // malformed line can't capture a runaway string, on a char boundary.
    let mut end = after_to
        .find(" (")
        .or_else(|| after_to.find(". "))
        .unwrap_or(after_to.len())
        .min(40);
    while end > 0 && !after_to.is_char_boundary(end) {
        end -= 1;
    }
    let model = after_to[..end].trim();
    if model.is_empty() || model.eq_ignore_ascii_case("None") {
        None
    } else {
        Some(model.to_string())
    }
}

/// Parse the start and end of an AGY conversation transcript to calculate total duration.
fn read_agy_duration(path: &Path) -> Option<u64> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut first_ts: Option<i64> = None;
    let mut last_ts: Option<i64> = None;

    for line in reader.lines().flatten() {
        if let Some(ts_str) = parse_created_at(&line) {
            if first_ts.is_none() {
                first_ts = parse_rfc3339_utc(&ts_str);
            }
            last_ts = parse_rfc3339_utc(&ts_str);
        }
    }

    if let (Some(start), Some(end)) = (first_ts, last_ts) {
        if end >= start {
            return Some((end - start) as u64 * 1000);
        }
    }
    None
}

/// Simple parser for created_at key in JSONL transcript lines.
fn parse_created_at(line: &str) -> Option<String> {
    let anchor = line.find("\"created_at\":\"")?;
    let start = anchor + "\"created_at\":\"".len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Format a ms duration compactly: `34s`, `12m`, `1h12m`.
pub fn fmt_dur(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m) = (s / 3600, (s % 3600) / 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// Parse an RFC3339 **UTC** timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z` — the shape both
/// the usage API and OMC's cache emit) into Unix epoch **seconds**. Fractional seconds
/// and the trailing `Z` are ignored; a non-UTC offset isn't expected from this source.
/// `None` on any malformed field, so a junk value just hides the countdown.
fn parse_rfc3339_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    // Require at least the fixed `YYYY-MM-DDTHH:MM:SS` prefix with its separators.
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        std::str::from_utf8(&b[r]).ok()?.parse::<i64>().ok()
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, min, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // days_from_civil (Howard Hinnant): civil date → days since 1970-01-01, no deps.
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + min * 60 + sec)
}

/// Format the time remaining until `reset_epoch` (Unix seconds, UTC) as a compact,
/// timezone-free countdown for the status bar: `2h14m`, `47m`, or `<1m` in the final
/// seconds. `None` once the reset is in the past (the window has rolled over and a
/// fresh figure is due) or the system clock is unreadable.
pub fn fmt_reset_in(reset_epoch: i64) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let rem = reset_epoch - now;
    if rem <= 0 {
        return None;
    }
    let mins = rem / 60;
    Some(if mins >= 60 {
        format!("{}h{:02}m", mins / 60, mins % 60)
    } else if mins >= 1 {
        format!("{mins}m")
    } else {
        "<1m".to_string()
    })
}

/// A compact one-line summary for Claude Code to show as its statusLine stdout.
pub fn render_line(obs: &SessionObs) -> String {
    let model = if obs.model.is_empty() {
        "—"
    } else {
        &obs.model
    };
    let ctx = if obs.ctx_tokens > 0 {
        format!("{}k ctx", obs.ctx_tokens / 1000)
    } else if obs.exceeds_200k {
        ">200k ctx".to_string()
    } else {
        "— ctx".to_string()
    };
    format!(
        "⌬ {model} · ◷ {} · {ctx} · ${:.2}",
        fmt_dur(obs.session_ms),
        obs.cost_usd
    )
}

/// Run the operator's **own** status line (their global `~/.claude/settings.json`
/// `statusLine.command`, e.g. the OMC HUD) with the same payload on stdin, and return
/// its stdout. We hijack `statusLine` per-session (via `--settings`) only to capture
/// obs, so chaining here preserves the operator's HUD in embedded terminals. `None`
/// when there is no global statusline command, it is ours (would recurse), or it
/// fails — the caller then prints our own compact line.
pub fn chain_statusline(input: &str) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let settings = claude_config_dir()?.join("settings.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings).ok()?).ok()?;
    let cmd = v.get("statusLine")?.get("command")?.as_str()?;
    // Never chain into ourselves.
    if cmd.contains("moonlight") && cmd.contains("statusline") {
        return None;
    }
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.as_mut()?.write_all(input.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
    (!line.is_empty()).then_some(line)
}

/// Append the `--settings '<file>'` flag to an app-launched `claude` command so CC
/// runs `moonlight statusline` (capturing obs) **only for app sessions**. The
/// settings file carries *only* `statusLine`, and `--settings` merges it on top of
/// the user's config — so their global statusline/hooks (OMC HUD, our gating) stay.
/// No-op (returns the command unchanged) if the binary path / support dir can't be
/// resolved, so a failure here never breaks a launch.
pub fn with_statusline(mut command: String) -> String {
    if let Some(flag) = statusline_settings_flag() {
        command.push(' ');
        command.push_str(&flag);
    }
    command
}

/// Write/refresh the statusline settings file and return its `--settings '<path>'`
/// flag (single-quoted — the support path contains a space). Rewritten each call so
/// the embedded `moonlight statusline` command tracks the current binary path.
fn statusline_settings_flag() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = support_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let settings_path = dir.join("cc-statusline-settings.json");
    let command = format!("'{}' statusline", exe.display());
    let body = serde_json::json!({
        "statusLine": { "type": "command", "command": command, "padding": 0 }
    });
    std::fs::write(&settings_path, serde_json::to_string_pretty(&body).ok()?).ok()?;
    Some(format!("--settings '{}'", settings_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_maps_statusline_fields() {
        let json = r#"{
            "session_id": "abc-123",
            "model": { "id": "claude-opus-4-8", "display_name": "Opus 4.8" },
            "output_style": { "name": "default" },
            "cost": { "total_duration_ms": 72000, "total_cost_usd": 0.42,
                      "total_lines_added": 10, "total_lines_removed": 3 },
            "exceeds_200k_tokens": false,
            "unknown_future": 1
        }"#;
        let (id, obs) = ingest(json, 999).unwrap();
        assert_eq!(id, "abc-123");
        assert_eq!(obs.model, "Opus 4.8");
        assert_eq!(obs.session_ms, 72000);
        assert_eq!(obs.persona, "default");
        assert_eq!(obs.cost_usd, 0.42);
        assert_eq!(obs.lines_added, 10);
        assert_eq!(obs.updated_ms, 999);
    }

    #[test]
    fn ingest_rejects_empty_and_idless_payloads() {
        assert!(ingest("not json", 0).is_none());
        assert!(ingest(r#"{"model":{"display_name":"x"}}"#, 0).is_none());
    }

    #[test]
    fn discover_picks_earliest_conversation_after_launch_in_root() {
        // AGY logs multiple lines per conversation; a launch correlates to the earliest
        // conversation *created* at/after its launch time, scoped to the session's root.
        let history = r#"
{"workspace":"/repo/a","conversationId":"old","timestamp":100}
{"workspace":"/repo/a","conversationId":"old","timestamp":300}
{"workspace":"/repo/a","conversationId":"mine","timestamp":250}
{"workspace":"/repo/a","conversationId":"mine","timestamp":900}
{"workspace":"/repo/b","conversationId":"other","timestamp":260}
{"workspace":"/repo/a","conversationId":"later","timestamp":500}
"#;
        // Launch at 200: `old` (created @100) predates it; `mine` (@250) is the earliest
        // conversation created after launch — even though a `mine` line at 900 is latest.
        assert_eq!(
            earliest_conversation_after(history, "/repo/a", 200),
            Some("mine".to_string())
        );
        // A different root doesn't leak in.
        assert_eq!(
            earliest_conversation_after(history, "/repo/b", 200),
            Some("other".to_string())
        );
        // Nothing created after a late launch → no correlation yet (retry next poll).
        assert_eq!(earliest_conversation_after(history, "/repo/a", 1000), None);
        // Malformed lines are skipped, not fatal.
        assert_eq!(
            earliest_conversation_after("garbage\n{}\n", "/repo/a", 0),
            None
        );
    }

    #[test]
    fn ctx_tokens_reads_latest_usage_from_transcript_tail() {
        let dir = std::env::temp_dir().join(format!("mlc-obs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        // Two assistant turns; the latest usage should win (prompt-side sum = 1500).
        let body = "\
{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":10,\"output_tokens\":7}}}
{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}
{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":1000,\"cache_read_input_tokens\":400,\"cache_creation_input_tokens\":100,\"output_tokens\":20}}}
";
        std::fs::write(&path, body).unwrap();
        assert_eq!(ctx_tokens_from_transcript(&path), Some(1500));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fmt_dur_is_compact() {
        assert_eq!(fmt_dur(3_000), "3s");
        assert_eq!(fmt_dur(72_000), "1m");
        assert_eq!(fmt_dur(4_320_000), "1h12m");
    }

    #[test]
    fn parse_quota_reads_omc_cache_shape() {
        let json = r#"{"timestamp":1,"data":{"fiveHourPercent":65,"fiveHourResetsAt":"x",
            "weeklyPercent":70,"weeklyResetsAt":"y","sonnetWeeklyPercent":0,
            "sonnetWeeklyResetsAt":null},"error":false}"#;
        let q = parse_quota(json).unwrap();
        assert_eq!(q.five_hour_pct, Some(65));
        assert_eq!(q.weekly_pct, Some(70));
        assert_eq!(q.sonnet_pct, Some(0));
        // An errored cache is treated as no data (don't show stale as fresh).
        assert!(parse_quota(r#"{"data":{},"error":true}"#).is_none());
    }

    #[test]
    fn ingest_prefers_native_context_window_block() {
        // Modern CC pipes `context_window`: the real window size (a 1M session is
        // not always marked in the model name), CC's own used %, and the current
        // prompt-side usage — all authoritative over our name-sniff + tail-scan.
        let json = r#"{
            "session_id": "abc",
            "model": { "id": "claude-opus-4-8", "display_name": "Opus 4.8" },
            "context_window": {
                "context_window_size": 1000000,
                "total_input_tokens": 230000,
                "used_percentage": 23.4,
                "current_usage": { "input_tokens": 2000,
                                   "cache_creation_input_tokens": 8000,
                                   "cache_read_input_tokens": 222000 }
            }
        }"#;
        let (_, obs) = ingest(json, 1).unwrap();
        assert_eq!(obs.ctx_limit, 1_000_000);
        assert_eq!(obs.ctx_tokens, 232_000); // current_usage sum wins
        assert_eq!(obs.ctx_used_pct, Some(23));
        assert_eq!(obs.ctx_pct(), Some(23)); // native % preferred end-to-end
    }

    #[test]
    fn ctx_pct_falls_back_to_tokens_over_window() {
        // Older CC: no native %, compute from tokens ÷ window; clamped at 100.
        let obs = SessionObs {
            ctx_tokens: 119_609,
            ctx_limit: 1_000_000,
            ..Default::default()
        };
        assert_eq!(obs.ctx_pct(), Some(11));
        let pegged = SessionObs {
            ctx_tokens: 250_000,
            ctx_limit: 200_000,
            ..Default::default()
        };
        assert_eq!(pegged.ctx_pct(), Some(100));
        // No data at all → `—`, never a bogus figure.
        assert_eq!(SessionObs::default().ctx_pct(), None);
    }

    #[test]
    fn context_limit_detects_1m_else_200k() {
        assert_eq!(context_limit("claude-opus-4-8", "Opus 4.8", false), 200_000);
        assert_eq!(
            context_limit("claude-sonnet-4-5[1m]", "Sonnet 4.5", false),
            1_000_000
        );
        // No `1m` marker, but CC reports >200k context — only a 1M window can do
        // that (the gauge would otherwise peg at 100% on a lightly-used session).
        assert_eq!(
            context_limit("claude-opus-4-8", "Opus 4.8", true),
            1_000_000
        );
    }

    #[test]
    fn session_obs_parses_snapshots_from_older_binaries() {
        // A pre-`ctx_limit` snapshot (real shape from disk) must still load —
        // dropping it would silently blank the whole obs row for that session.
        let json = r#"{"model":"Opus 4.8","session_ms":1,"cost_usd":0.0,"lines_added":0,
            "lines_removed":0,"persona":"default","ctx_tokens":105918,
            "exceeds_200k":false,"updated_ms":1}"#;
        let obs: SessionObs = serde_json::from_str(json).unwrap();
        assert_eq!(obs.ctx_tokens, 105_918);
        assert_eq!(obs.ctx_limit, 0); // defaulted → the bar shows "—", not a bogus %
    }

    #[test]
    fn parse_usage_api_maps_windows() {
        // The self-fetch endpoint: each window's `utilization` is a 0–100 %, rounded.
        let json = br#"{"five_hour":{"utilization":65.4,"resets_at":"2026-06-08T12:10:00.735Z"},
            "seven_day":{"utilization":70},"seven_day_sonnet":{"utilization":0}}"#;
        let q = parse_usage_api(json).unwrap();
        assert_eq!(q.five_hour_pct, Some(65));
        assert_eq!(q.five_hour_resets_at, Some(1_780_920_600)); // 2026-06-08T12:10:00Z
        assert_eq!(q.weekly_pct, Some(70));
        assert_eq!(q.sonnet_pct, Some(0));
        // An empty/figureless response is treated as no data.
        assert!(parse_usage_api(b"{}").is_none());
    }

    #[test]
    fn parse_rfc3339_utc_handles_window_resets() {
        // The fractional `.fff` and the `Z` are ignored; result is epoch seconds.
        assert_eq!(
            parse_rfc3339_utc("2026-06-08T12:10:00.735Z"),
            Some(1_780_920_600)
        );
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
        // Malformed input hides the countdown rather than panicking.
        assert_eq!(parse_rfc3339_utc("x"), None);
        assert_eq!(parse_rfc3339_utc("2026/06/08 12:10:00"), None);
    }

    #[test]
    fn parses_agy_model_from_settings_change() {
        let line =
            "stuff `Model Selection` from None to Gemini 3.5 Flash (Medium). No need to comment.";
        assert_eq!(parse_agy_model(line).as_deref(), Some("Gemini 3.5 Flash"));
        // No reasoning-level suffix → ends at the sentence period.
        assert_eq!(
            parse_agy_model("`Model Selection` from None to Gemini 3 Pro. ok").as_deref(),
            Some("Gemini 3 Pro")
        );
        // No marker / unset model → nothing (untrusted input never panics).
        assert_eq!(parse_agy_model("no model here"), None);
        assert_eq!(parse_agy_model("Model Selection to None (x)"), None);
    }

    #[test]
    fn read_agy_duration_calculates_interval_from_transcript_endpoints() {
        let dir = std::env::temp_dir().join(format!("mlc-obs-dur-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dur_t.jsonl");
        let body = "\
{\"created_at\":\"2026-06-25T19:00:00Z\",\"type\":\"user\",\"message\":{\"content\":\"hi\"}}
{\"created_at\":\"2026-06-25T19:15:30Z\",\"type\":\"assistant\",\"message\":{\"content\":\"hello\"}}
";
        std::fs::write(&path, body).unwrap();
        // 15m 30s = 930s = 930_000ms
        assert_eq!(read_agy_duration(&path), Some(930_000));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
