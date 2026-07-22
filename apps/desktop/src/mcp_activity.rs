//! Per-**MCP-server** activity for the Services view, mined from the session
//! transcripts (the same `~/.claude/projects/<proj>/<id>.jsonl` files the history
//! reads). Claude Code records every tool call as a `tool_use` block named
//! `mcp__<server>__<tool>`, so the transcript is the ground truth for *which* MCP
//! servers a session actually used and *how much* — richer than the moonlight-only
//! audit log, which can't see third-party servers (figma, phoenix, …).
//!
//! GPUI-free and pure where it matters: [`extract_calls`], [`merge`] and
//! [`parse_rfc3339_millis`] are unit-tested without a filesystem; [`build`] is the
//! thin fs orchestration the panel's background poll calls (it resolves transcript
//! paths, reads changed files only via the caller's `ScanCache`, and folds in the
//! **accessible** servers discovered from config so idle-but-configured servers still
//! show up).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Cap on the per-server "recent activity" tail.
const RECENT_CAP: usize = 12;
/// Cap on the per-server tool breakdown.
const TOOLS_CAP: usize = 14;

/// One MCP tool call observed in a transcript.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct McpCall {
    /// Epoch millis of the call (0 when the line carried no parseable timestamp).
    pub at_millis: i64,
    pub server: String,
    /// The tool name with the `mcp__<server>__` prefix stripped.
    pub tool: String,
    /// The 8-char short id of the session that made the call.
    pub session_short: String,
}

/// A session's usage of one server.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionUse {
    pub short_id: String,
    pub name: Option<String>,
    pub calls: usize,
    pub last_used: i64,
}

/// One MCP server as the Services view shows it: config facts (accessible/transport)
/// merged with observed activity (calls, sessions, tools, recent tail).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct McpServerView {
    pub server: String,
    /// Transport from config (`http` / `stdio` / explicit `type`), if discovered.
    pub transport: Option<String>,
    /// Present in an MCP config (so "added" to sessions), even with zero activity.
    pub accessible: bool,
    pub total_calls: usize,
    pub last_used: i64,
    /// Sessions that used it, newest-active first.
    pub sessions: Vec<SessionUse>,
    /// `(tool, count)` breakdown, most-called first.
    pub tools: Vec<(String, usize)>,
    /// Recent calls across all sessions, newest first.
    pub recent: Vec<McpCall>,
}

/// The panel's re-parse cache: `short_id → (mtime_secs, len, calls)`. Unchanged
/// transcripts are reused instead of re-read; the caller owns it across polls.
pub type ScanCache = HashMap<String, (u64, u64, Vec<McpCall>)>;

/// Parse an RFC3339 UTC timestamp like `2026-07-01T08:23:31.636Z` to epoch millis.
/// Fixed-offset field slicing (the transcript format is stable); returns `None` on
/// anything that doesn't look like that shape.
pub fn parse_rfc3339_millis(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z).and_then(|x| x.parse::<i64>().ok());
    let (year, mon, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mon) || !(1..=31).contains(&day) {
        return None;
    }
    // Optional `.fff` fractional seconds → millis (pad/truncate to 3 digits).
    let mut millis = 0i64;
    if s.as_bytes().get(19) == Some(&b'.') {
        let mut frac: String = s[20..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .take(3)
            .collect();
        while frac.len() < 3 {
            frac.push('0');
        }
        millis = frac.parse().unwrap_or(0);
    }
    let days = days_from_civil(year, mon, day);
    Some(((days * 24 + hh) * 3600 + mm * 60 + ss) * 1000 + millis)
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm — correct
/// across leap years, no chrono dep).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Extract every `mcp__<server>__<tool>` call from a transcript's JSONL text, tagged
/// with `session_short`. A cheap `contains("mcp__")` pre-filter skips the many lines
/// that can't match before the JSON parse.
pub fn extract_calls(jsonl: &str, session_short: &str) -> Vec<McpCall> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        if !line.contains("mcp__") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let at = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(parse_rfc3339_millis)
            .unwrap_or(0);
        let Some(arr) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let Some(name) = block.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let Some(rest) = name.strip_prefix("mcp__") else {
                continue;
            };
            let Some((server, tool)) = rest.split_once("__") else {
                continue;
            };
            out.push(McpCall {
                at_millis: at,
                server: server.to_string(),
                tool: tool.to_string(),
                session_short: session_short.to_string(),
            });
        }
    }
    out
}

/// Fold calls + session names + the accessible (configured) servers into the sorted
/// server views. Active servers (calls > 0) come first (newest-used first), then idle
/// accessible servers by name.
pub fn merge(
    mut calls: Vec<McpCall>,
    names: &HashMap<String, Option<String>>,
    accessible: &[(String, Option<String>)],
) -> Vec<McpServerView> {
    // Newest-first so each server's `recent` tail is just the first RECENT_CAP hits.
    calls.sort_by(|a, b| b.at_millis.cmp(&a.at_millis));

    let mut views: BTreeMap<String, McpServerView> = BTreeMap::new();
    for (name, transport) in accessible {
        let v = views.entry(name.clone()).or_default();
        v.server = name.clone();
        v.accessible = true;
        v.transport = transport.clone();
    }

    let mut tool_counts: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut sess: HashMap<String, HashMap<String, (usize, i64)>> = HashMap::new();
    for c in &calls {
        let v = views.entry(c.server.clone()).or_default();
        v.server = c.server.clone();
        v.total_calls += 1;
        v.last_used = v.last_used.max(c.at_millis);
        if v.recent.len() < RECENT_CAP {
            v.recent.push(c.clone());
        }
        *tool_counts
            .entry(c.server.clone())
            .or_default()
            .entry(c.tool.clone())
            .or_default() += 1;
        let s = sess
            .entry(c.server.clone())
            .or_default()
            .entry(c.session_short.clone())
            .or_insert((0, 0));
        s.0 += 1;
        s.1 = s.1.max(c.at_millis);
    }

    let mut out: Vec<McpServerView> = views
        .into_values()
        .map(|mut v| {
            if let Some(tc) = tool_counts.remove(&v.server) {
                let mut t: Vec<(String, usize)> = tc.into_iter().collect();
                t.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                t.truncate(TOOLS_CAP);
                v.tools = t;
            }
            if let Some(sm) = sess.remove(&v.server) {
                let mut s: Vec<SessionUse> = sm
                    .into_iter()
                    .map(|(short, (calls, last))| SessionUse {
                        name: names.get(&short).cloned().flatten(),
                        short_id: short,
                        calls,
                        last_used: last,
                    })
                    .collect();
                s.sort_by(|a, b| b.last_used.cmp(&a.last_used).then(b.calls.cmp(&a.calls)));
                v.sessions = s;
            }
            v
        })
        .collect();

    out.sort_by(|a, b| {
        (b.total_calls > 0)
            .cmp(&(a.total_calls > 0))
            .then(b.last_used.cmp(&a.last_used))
            .then_with(|| a.server.cmp(&b.server))
    });
    out
}

/// The servers configured (and thus "added") for the given project roots: the
/// moonlight server (always injected), plus `mcpServers` from `~/.claude.json`
/// (global + per-project) and each `<root>/.mcp.json`. Best-effort — missing or
/// malformed files simply contribute nothing.
pub fn accessible_servers(roots: &[PathBuf]) -> Vec<(String, Option<String>)> {
    let mut map: BTreeMap<String, Option<String>> = BTreeMap::new();
    map.insert("moonlight".to_string(), Some("http".to_string()));

    if let Some(home) = std::env::var_os("HOME") {
        let global = PathBuf::from(&home).join(".claude.json");
        if let Ok(text) = std::fs::read_to_string(&global) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                merge_servers_obj(v.get("mcpServers"), &mut map);
                if let Some(projects) = v.get("projects").and_then(|p| p.as_object()) {
                    for root in roots {
                        let key = root.to_string_lossy();
                        if let Some(pc) = projects.get(key.as_ref()) {
                            merge_servers_obj(pc.get("mcpServers"), &mut map);
                        }
                    }
                }
            }
        }
    }
    for root in roots {
        if let Ok(text) = std::fs::read_to_string(root.join(".mcp.json")) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                merge_servers_obj(v.get("mcpServers"), &mut map);
            }
        }
    }
    map.into_iter().collect()
}

/// Merge one `mcpServers` JSON object's names + inferred transport into `out`.
fn merge_servers_obj(obj: Option<&serde_json::Value>, out: &mut BTreeMap<String, Option<String>>) {
    let Some(obj) = obj.and_then(|m| m.as_object()) else {
        return;
    };
    for (name, cfg) in obj {
        let transport = cfg
            .get("type")
            .and_then(|t| t.as_str())
            .map(String::from)
            .or_else(|| cfg.get("url").map(|_| "http".to_string()))
            .or_else(|| cfg.get("command").map(|_| "stdio".to_string()));
        out.entry(name.clone()).or_insert(transport);
    }
}

/// Build the per-server activity for the panel: resolve each session's transcript,
/// re-parse only files whose (mtime, len) changed (via `cache`), and merge with the
/// accessible servers discovered from `roots`. `sessions` is `(full_id, title)`.
pub fn build(
    sessions: &[(String, Option<String>)],
    roots: &[PathBuf],
    cache: &mut ScanCache,
) -> Vec<McpServerView> {
    let accessible = accessible_servers(roots);
    let mut all_calls: Vec<McpCall> = Vec::new();
    let mut names: HashMap<String, Option<String>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (id, name) in sessions {
        let short: String = id.chars().take(8).collect();
        names.insert(short.clone(), name.clone());
        seen.insert(short.clone());
        let Some(path) = crate::transcript::transcript_path(id) else {
            continue;
        };
        let (mtime, len) = file_stamp(&path);
        let calls = match cache.get(&short) {
            Some((cm, cl, c)) if *cm == mtime && *cl == len => c.clone(),
            _ => {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let c = extract_calls(&text, &short);
                cache.insert(short.clone(), (mtime, len, c.clone()));
                c
            }
        };
        all_calls.extend(calls);
    }
    cache.retain(|k, _| seen.contains(k));
    merge(all_calls, &names, &accessible)
}

/// `(mtime_secs, len)` for a file, `(0, 0)` when it can't be stat'd.
fn file_stamp(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (mtime, m.len())
        }
        Err(_) => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rfc3339_millis_reads_the_transcript_format() {
        // Epoch itself.
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:00.000Z"), Some(0));
        // One full day + 1ms.
        assert_eq!(
            parse_rfc3339_millis("1970-01-02T00:00:00.001Z"),
            Some(86_400_000 + 1)
        );
        // A real sample; fractional millis honoured, trailing Z ignored.
        let a = parse_rfc3339_millis("2026-07-01T08:23:31.636Z").unwrap();
        let b = parse_rfc3339_millis("2026-07-01T08:23:31.000Z").unwrap();
        assert_eq!(a - b, 636);
        // Leap day parses (2024 is a leap year).
        assert!(parse_rfc3339_millis("2024-02-29T12:00:00Z").is_some());
        // Garbage → None.
        assert_eq!(parse_rfc3339_millis("nope"), None);
        assert_eq!(parse_rfc3339_millis("2026-13-01T00:00:00Z"), None);
    }

    const SAMPLE: &str = r#"{"type":"assistant","timestamp":"2026-07-01T10:00:00.000Z","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"mcp__phoenix__run_select_query","input":{}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}
{"type":"assistant","timestamp":"2026-07-01T10:00:05.000Z","message":{"content":[{"type":"tool_use","name":"mcp__moonlight__request_phase"},{"type":"tool_use","name":"mcp__phoenix__run_select_query"}]}}
{"type":"assistant","timestamp":"2026-07-01T10:00:09.000Z","message":{"content":[{"type":"tool_use","name":"Read"}]}}"#;

    #[test]
    fn extract_calls_finds_mcp_tool_uses_only() {
        let calls = extract_calls(SAMPLE, "sess1234");
        // 3 mcp calls (2 phoenix, 1 moonlight); the plain `Read` + tool_result skipped.
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|c| c.session_short == "sess1234"));
        let phoenix: Vec<_> = calls.iter().filter(|c| c.server == "phoenix").collect();
        assert_eq!(phoenix.len(), 2);
        assert_eq!(phoenix[0].tool, "run_select_query");
        let moon = calls.iter().find(|c| c.server == "moonlight").unwrap();
        assert_eq!(moon.tool, "request_phase");
        assert_eq!(
            moon.at_millis,
            parse_rfc3339_millis("2026-07-01T10:00:05.000Z").unwrap()
        );
    }

    #[test]
    fn merge_groups_by_server_with_tools_sessions_and_order() {
        let calls = extract_calls(SAMPLE, "sess1234");
        let mut names = HashMap::new();
        names.insert("sess1234".to_string(), Some("My Session".to_string()));
        // `terraform` is accessible but idle; moonlight always accessible.
        let accessible = vec![
            ("moonlight".to_string(), Some("http".to_string())),
            ("terraform".to_string(), Some("stdio".to_string())),
        ];
        let views = merge(calls, &names, &accessible);

        let phoenix = views.iter().find(|v| v.server == "phoenix").unwrap();
        assert_eq!(phoenix.total_calls, 2);
        assert_eq!(phoenix.tools, vec![("run_select_query".to_string(), 2)]);
        assert_eq!(phoenix.sessions.len(), 1);
        assert_eq!(phoenix.sessions[0].name.as_deref(), Some("My Session"));
        assert_eq!(phoenix.sessions[0].calls, 2);
        assert!(
            !phoenix.accessible,
            "phoenix only seen in transcript, not config"
        );

        let terraform = views.iter().find(|v| v.server == "terraform").unwrap();
        assert!(terraform.accessible && terraform.total_calls == 0);

        // Active servers sort before idle ones: terraform (idle) is last.
        assert_eq!(views.last().unwrap().server, "terraform");
        // moonlight is both accessible and active.
        let moon = views.iter().find(|v| v.server == "moonlight").unwrap();
        assert!(moon.accessible && moon.total_calls == 1);
    }
}
