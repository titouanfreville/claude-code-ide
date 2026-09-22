//! Claude observability **read model** — the per-session snapshots and the account
//! usage quota, shared by every MoonlightCode process.
//!
//! Claude Code runs a `statusLine` command on every render and pipes a JSON snapshot
//! at it; `apps/desktop`'s `obs` module parses that payload and drops a small
//! `<support>/obs/<id>.json` (see [`write_obs`]). Everything that *reads* those
//! snapshots back — the desktop status bar, and the headless control API that serves
//! the VSCode client — lives here instead, so the two surfaces can never disagree
//! about what "42% of the 5-hour window" means.
//!
//! The account quota is not in the statusline payload at all: it is fetched straight
//! from Anthropic's OAuth usage endpoint with Claude Code's own token ([`quota`]),
//! falling back to oh-my-claudecode's cache. That fetch is network-bound — call it on
//! a slow cadence and cache the result.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::support::support_dir;

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

/// Claude Code's config dir (`$CLAUDE_CONFIG_DIR`, else `~/.claude`).
pub fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude"))
}

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

/// The user's Claude OAuth access token — the same token Claude Code itself uses.
/// Tries `claudeAiOauth.accessToken` in `~/.claude/.credentials.json` first (Linux,
/// or any platform where CC was told to use file storage), then the **macOS
/// Keychain** (`security find-generic-password`), which is where current macOS
/// Claude Code installs store credentials instead of writing the file at all.
fn oauth_access_token() -> Option<String> {
    claude_config_dir()
        .and_then(|dir| oauth_access_token_from(&dir))
        .or_else(oauth_access_token_from_keychain)
}

/// Parse the OAuth access token out of a `.credentials.json` living in `config_dir`.
/// Split from [`oauth_access_token`] so the parsing logic is unit-testable without
/// touching the real `$HOME`/`$CLAUDE_CONFIG_DIR`.
fn oauth_access_token_from(config_dir: &Path) -> Option<String> {
    let path = config_dir.join(".credentials.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(str::to_owned)
}

/// Claude Code's macOS Keychain service name for its OAuth credentials.
#[cfg(target_os = "macos")]
const CC_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Read the OAuth token from the macOS Keychain — same JSON shape as
/// `.credentials.json`, just stored as the Keychain item's password instead of a
/// file. `None` on any failure (no item, access denied, unparsable) so a Keychain
/// prompt/denial never blocks the caller; it just falls through to the OMC cache.
#[cfg(target_os = "macos")]
fn oauth_access_token_from_keychain() -> Option<String> {
    use std::process::{Command, Stdio};
    let out = Command::new("security")
        .args(["find-generic-password", "-w", "-s", CC_KEYCHAIN_SERVICE])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    parse_keychain_credentials(raw.trim())
}

#[cfg(not(target_os = "macos"))]
fn oauth_access_token_from_keychain() -> Option<String> {
    None
}

/// Parse the Keychain item's password (same `claudeAiOauth.accessToken` shape as
/// `.credentials.json`) into the access token. Split out for unit testing.
#[cfg(target_os = "macos")]
fn parse_keychain_credentials(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
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
pub fn parse_rfc3339_utc(s: &str) -> Option<i64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_dur_is_compact() {
        assert_eq!(fmt_dur(3_000), "3s");
        assert_eq!(fmt_dur(72_000), "1m");
        assert_eq!(fmt_dur(4_320_000), "1h12m");
    }

    #[test]
    fn oauth_access_token_reads_credentials_file() {
        let dir = std::env::temp_dir().join(format!("mlc-obs-creds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"sk-test-token","refreshToken":"r"}}"#,
        )
        .unwrap();
        assert_eq!(
            oauth_access_token_from(&dir),
            Some("sk-test-token".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oauth_access_token_none_on_missing_or_malformed() {
        // No `.credentials.json` at all.
        let empty_dir =
            std::env::temp_dir().join(format!("mlc-obs-nocreds-{}", std::process::id()));
        std::fs::create_dir_all(&empty_dir).unwrap();
        assert_eq!(oauth_access_token_from(&empty_dir), None);
        let _ = std::fs::remove_dir_all(&empty_dir);

        // Present but missing the expected shape.
        let bad_dir = std::env::temp_dir().join(format!("mlc-obs-badcreds-{}", std::process::id()));
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join(".credentials.json"), r#"{"unexpected":true}"#).unwrap();
        assert_eq!(oauth_access_token_from(&bad_dir), None);
        let _ = std::fs::remove_dir_all(&bad_dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_keychain_credentials_reads_token() {
        let raw = r#"{"claudeAiOauth":{"accessToken":"sk-keychain-token","refreshToken":"r"}}"#;
        assert_eq!(
            parse_keychain_credentials(raw),
            Some("sk-keychain-token".to_string())
        );
        assert_eq!(parse_keychain_credentials("not json"), None);
        assert_eq!(parse_keychain_credentials(r#"{"unexpected":true}"#), None);
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
}
