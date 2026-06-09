# Claude Code Integration Facts — API, Hooks, Networking

**Date:** 2026-06-08 · **Source:** Official CC docs (network-config, hooks, cli-reference, permissions, tools-reference)

## Executive Summary

Claude Code respects standard proxies (`ANTHROPIC_BASE_URL`, `HTTPS_PROXY`) for ALL model traffic, but:
- **Hooks cannot modify content** (only deny/allow/observe)
- **PostToolUse hooks fire too late** for secret redaction (tool output already captured)
- **Rate-limit headers are invisible** to CC (must intercept at reverse proxy)
- **Tool steering has no preference mechanism** (only deny rules or CLAUDE.md guidance)

→ **Your reverse proxy is the trust boundary** for secrets + quota visibility.

---

## 1. API Routing & Networking

### ANTHROPIC_BASE_URL
- **Respected:** YES — all model traffic routes through custom endpoint
- **Auth:** Standard `Authorization: Bearer <key>` header
- **Content:** ALL reaches the proxy:
  - System prompt
  - User messages
  - Tool results
  - File contents (via Read/Grep)
  - API key in auth header
- **Gotchas:**
  - Try both with/without trailing slash on the base URL
  - Streaming works transparently
  - Retries use standard HTTP retry semantics

### HTTP_PROXY / HTTPS_PROXY
- **Supported:** Both standard proxies work
- **Precedence:** Environment variable applies to all HTTP(S) calls
- **Basic auth:** `HTTPS_PROXY=http://user:pass@proxy.com:8080` supported
- **NO_PROXY:** Space or comma-separated; `*` bypasses all
- **Not supported:** SOCKS proxies
- **Platform:** Works on macOS, Linux, Windows; WSL respects both

### Certificate Trust
Default: Bundled Mozilla CAs **+** OS certificate store
- TLS-inspection proxies (CrowdStrike, Zscaler): their root CA must be in OS trust store
- Custom CA: `NODE_EXTRA_CA_CERTS=/path/to/ca.pem`
- Control via: `CLAUDE_CODE_CERT_STORE=bundled` or `system` (default both)

### mTLS Authentication
- Client cert: `CLAUDE_CODE_CLIENT_CERT=/path/to/cert.pem`
- Private key: `CLAUDE_CODE_CLIENT_KEY=/path/to/key.pem`
- Passphrase: `CLAUDE_CODE_CLIENT_KEY_PASSPHRASE="..."`

---

## 2. Hook Content Modification: What's Possible

### Hook Events & Capabilities

| Event | Can Block? | Can Modify Content? | Can Add Context? | Notes |
|-------|-----------|-------------------|-----------------|-------|
| **UserPromptSubmit** | ✅ Yes (exit 2) | ❌ No | ✅ Yes (additionalContext) | Can reject before Claude sees it |
| **PreToolUse** | ✅ Yes (permissionDecision) | ❌ No tool input rewrite | ✅ Feedback | Can deny but not rewrite commands |
| **PostToolUse** | ❌ No (too late) | ❌ No output rewrite | ✅ Feedback | Tool already executed; output captured |
| **Stop** | ✅ Yes (continue:false) | ❌ No | ✅ Feedback | Can prevent response from sending |

### JSON Contract

**UserPromptSubmit** (exit 0):
```json
{
  "continue": true|false,
  "stopReason": "string (if continue: false)",
  "hookSpecificOutput": {
    "additionalContext": "string added to Claude's next turn"
  }
}
```

**PreToolUse** (exit 0):
```json
{
  "hookSpecificOutput": {
    "permissionDecision": "allow|deny|ask|defer",
    "permissionDecisionReason": "string"
  }
}
```

**PostToolUse** (exit 0):
```json
{
  "hookSpecificOutput": {
    "additionalContext": "string added to Claude's next message"
  }
}
```

Exit codes:
- **0**: Success; parse JSON
- **2**: Block action; show stderr
- **Other**: Non-blocking error; show but continue

### Critical Limitation for Secret Redaction

**PostToolUse hooks cannot redact secrets.** Flow:
1. Claude calls `Read ~/.env` containing API keys
2. Tool executes in CC harness; full output captured
3. PostToolUse hook fires (too late—secrets already in CC's context)
4. Claude sees unredacted output and sends it to the API

**Workaround:** Intercept at the reverse proxy level, *before* the request leaves the machine.

---

## 3. Rate-Limit & Quota Visibility

### Not Exposed to CC
- Rate-limit headers are **NOT** visible to hooks
- Token usage is **NOT** visible in statusline
- Context window % is **NOT** exposed
- Quota/headroom signals are **NOT** available

### What IS Available
- **`/context` command** (interactive only, not hookable): shows what's in context
- **`/mcp` command**: per-server token costs
- **Agent SDK response**: `/v1/messages` response includes `usage` object (input_tokens, output_tokens, cache tokens)
- **HTTP response headers**: Rate-limit headers visible at your reverse proxy (RateLimit-Limit-Requests, RateLimit-Remaining-Requests, RateLimit-Reset-Requests)

### Implementation
You must:
1. Intercept HTTP responses at the reverse proxy
2. Parse rate-limit headers
3. Expose locally (metrics endpoint, file, IPC) for your IDE to poll
4. Or enforce quota client-side (queue sessions, delay launches)

---

## 4. CLI Flags: `--append-system-prompt` & `--mcp-config`

### `--append-system-prompt`
- **Exists:** YES
- **Behavior:** Appends text to the END of the default system prompt
- **Persistent:** NO — per-invocation only, does not mutate CLAUDE.md
- **Format:** Single-quoted shell arg (newlines must be escaped; single quotes inside will break the arg)
- **Precedence:**
  1. Default CC system prompt
  2. CLAUDE.md content
  3. `--append-system-prompt` text (appended last)
- **Paired flag:** `--append-system-prompt-file <path>` loads from a file

**Use case for MoonlightCode:** Inject phase/MCP guidance without mutating project CLAUDE.md:
```bash
claude --append-system-prompt "You are in Discovery phase. Use request_phase to change phases."
```

### `--mcp-config`
- **Exists:** YES
- **Behavior:** Loads MCP servers from JSON files or inline JSON (space-separated)
- **Format:** File path or inline JSON string
- **Persistent:** NO — per-session only
- **Precedence:** Merges with project `.mcp.json` and managed settings (unless `--strict-mcp-config` used)
- **Paired flag:** `--strict-mcp-config` to ONLY use servers from this flag

**Example:**
```bash
claude --mcp-config '{"mcpServers":{"moonlight":{"type":"http","url":"http://127.0.0.1:5555/mcp"}}}'
```

**Use case for MoonlightCode:** Inject the per-session MCP endpoint at launch without requiring project-level `.mcp.json` edits.

---

## 5. Tool Steering: Disabling Built-Ins & Biasing toward MCP

### What's Possible

**Hard denial (remove from context):**
```json
{
  "permissions": {
    "deny": ["Read", "Grep", "Bash"]
  }
}
```
Tool is removed from Claude's available tools entirely; Claude never sees it.

**Scoped denial:**
```json
{
  "permissions": {
    "deny": ["Read(*.log)", "Read(~/.aws/**)", "Bash(rm *)"]
  }
}
```
Tool is available but specific uses are blocked.

**CLI flag equivalents:**
```bash
claude --disallowedTools "Read" "Grep"
claude --allowedTools "Read(src/**)" "Bash(npm *)"
```

### What's NOT Possible

❌ **No "prefer MCP tools" setting** — CC doesn't know which tools are built-in vs. MCP
❌ **No tool priority/weighting** — model decides based on context
❌ **No hook-based tool redirection** — can't say "on Read, use MCP read instead"
❌ **No disallowedTools in PreToolUse hooks** — only static config

### Soft Steering (Guidance, Not Enforcement)

**CLAUDE.md guidance:**
```markdown
For file reads, prefer the compact `mcp__moonlight__read_compact` tool to reduce token usage.
```
- Shapes Claude's reasoning but doesn't enforce
- Model may ignore it for legitimate reasons
- Works if Claude sees both tools as viable

**PreToolUse hook advisory:**
```bash
# Can deny Read and return feedback with MCP alternative
{
  "hookSpecificOutput": {
    "permissionDecision": "deny",
    "permissionDecisionReason": "Use mcp__moonlight__read_compact instead for token economy"
  }
}
```
- Prompts user for each Read attempt
- Not automatic; user must approve the alternative

### Recommended Approach for Token Economy

1. **Deny aggressive patterns** (if safe for your domain):
   ```json
   {
     "permissions": {
       "deny": ["Read(**/*.log)", "Grep(**/*.txt)"]
     }
   }
   ```

2. **Add CLAUDE.md guidance** mentioning compact MCP verbs + their names

3. **Monitor usage** via `rtk cargo test` + your reverse proxy quota endpoint

4. **(Don't rely on) PreToolUse hooks to soft-deny** — too noisy if every read prompts

---

## 6. Session Launch Flags: Precedence & Composition

**Your wiring pattern (good):**
```bash
claude \
  --append-system-prompt "$(cat ide-context.txt)" \
  --mcp-config "$(cat mcp-config.json)" \
  --session-id <uuid> \
  --permission-mode plan \
  [other flags]
```

Both flags compose safely:
- System prompt appended after CLAUDE.md
- MCP config merges with project settings
- No conflicts; applied in order

---

## 7. Secret Redaction: Architecture Recommendation

Since **PostToolUse hooks can't redact**, build it at the reverse proxy:

```
CC Session
  ↓ HTTPS_PROXY=http://127.0.0.1:9999
Redaction Proxy (your code)
  ├─ Parse request body (JSON messages array)
  ├─ Apply redaction rules (regex, entropy, API key shapes)
  ├─ Rewrite request before forwarding
  ├─ Log violations locally
  ├─ Forward to api.anthropic.com (or your upstream)
  ├─ Parse response
  ├─ Capture rate-limit headers for quota tracking
  └─ Return response to CC
```

**Benefits:**
- Catches secrets BEFORE they leave the machine
- Sees ALL traffic (not just tool output)
- Access to response headers (rate limits)
- Can implement quota enforcement (queue, delay)
- Can audit/log violations

**Reference:** Your MCP actor already injects `--mcp-config` per session; the proxy is the natural next layer.

---

## Files & References

**Official Documentation:**
- [Enterprise network configuration](https://code.claude.com/docs/en/network-config.md) — proxy, CA, mTLS
- [Hooks reference](https://code.claude.com/docs/en/hooks.md) — event types, JSON contracts, examples
- [CLI reference](https://code.claude.com/docs/en/cli-reference.md) — `--append-system-prompt`, `--mcp-config`, all flags
- [Permissions](https://code.claude.com/docs/en/permissions.md) — deny rules, tool-specific patterns
- [Tools reference](https://code.claude.com/docs/en/tools-reference.md) — complete tool list, availability

**Relevant to MoonlightCode:**
- `~/.claude/CLAUDE.md` — user global instructions
- `.claude/settings.json` — project settings (shared)
- `.claude/settings.local.json` — personal overrides
- `CLAUDE.md` — project conventions (checked in)

---

## Known Gaps / Limitations

1. **No rate-limit visibility in CC** → Build at reverse proxy
2. **No hook-based content modification** → Intercept at reverse proxy or PreToolUse deny + feedback
3. **No tool preference mechanism** → Deny + CLAUDE.md guidance
4. **No PostToolUse output redaction** → Reverse proxy only
5. **Manual `/clear` drifts session ID** → CC design (use Reset button instead)

---

## Quick Checklist for MoonlightCode Integration

- [ ] Reverse proxy routes `ANTHROPIC_BASE_URL=http://127.0.0.1:YOUR_PORT`
- [ ] Proxy captures and redacts secrets before forwarding to api.anthropic.com
- [ ] Proxy exposes rate-limit headers to IDE (metrics endpoint or file)
- [ ] `--append-system-prompt` injected at every managed launch (phase/MCP guidance)
- [ ] `--mcp-config` injected with per-session HTTP endpoint
- [ ] Permission deny rules lock down sensitive paths
- [ ] CLAUDE.md guidance points to compact MCP verbs
- [ ] Audit log captures all CC tool calls + MCP verbs
