# Claude Code API Integration Research — 2026-06-08

**Status:** Complete · **Scope:** API routing, hooks, networking, tool control for MoonlightCode's IDE orchestration

## Findings Summary

### ✅ What Works
1. **All model traffic routes through `ANTHROPIC_BASE_URL`** — system prompt, user messages, tool results, file contents, API key
2. **`HTTP_PROXY` / `HTTPS_PROXY` respected** — standard environment variables work transparently
3. **`--append-system-prompt` injected per-session** — guidance only, doesn't mutate CLAUDE.md
4. **`--mcp-config` injected per-session** — wires your moonlight MCP server at launch
5. **Permission deny rules work** — can lock down Read/Grep on sensitive paths

### ❌ Hard Blockers (No Native Solution)

| Problem | Why | Solution |
|---------|-----|----------|
| Secrets in tool output reach the API | PostToolUse hooks fire AFTER output is captured | Intercept at reverse proxy (parse request JSON, redact before forward) |
| Rate-limit headers invisible to CC | No hook exposure, no statusline API | Parse HTTP responses at reverse proxy, expose locally |
| Can't prefer MCP tools over built-ins | CC model doesn't know tool origin; no preference mechanism | Deny built-in Read + CLAUDE.md guidance (soft) |
| PreToolUse can't rewrite tool input | Hooks only allow/deny, no modification | Use deny + feedback, or intercept at proxy level |

### 🏗️ Recommended Architecture

```
CC Session (ANTHROPIC_BASE_URL=http://127.0.0.1:9999, HTTPS_PROXY=...)
    ↓
Your Redaction Proxy Sidecar
  ├─ Parse POST body (JSON messages)
  ├─ Apply secret redaction (regex, entropy, API key shapes)
  ├─ Capture rate-limit headers from response
  ├─ Expose quota to IDE (metrics endpoint, .state file, or IPC)
  └─ Forward to api.anthropic.com
```

**Why this works:**
- Catches secrets BEFORE they leave the machine
- Single point of control for all model traffic
- Access to response headers for quota/rate-limit tracking
- Can enforce quota client-side (queue sessions, delay launches)
- Complements your existing MCP actor + PDP gate

### 📋 Integration Checklist

- [ ] Reverse proxy binds on `127.0.0.1:9999` (or your chosen port)
- [ ] Proxy parses `/v1/messages` request JSON, applies redaction rules
- [ ] Proxy captures `RateLimit-*` headers from API responses
- [ ] IDE polls proxy (or reads state file) for quota headroom
- [ ] `--append-system-prompt` carries phase + MCP guidance at every launch
- [ ] `--mcp-config` carries per-session HTTP endpoint URL
- [ ] `.claude/settings.json` deny rules lock sensitive paths (`.env`, `~/.aws/**`, etc.)
- [ ] CLAUDE.md mentions compact MCP verbs for token economy
- [ ] Audit log in moonlight.db records all CC tool calls + MCP verbs

### 🔗 Key References

- **Network config:** https://code.claude.com/docs/en/network-config.md
- **Hooks:** https://code.claude.com/docs/en/hooks.md (PostToolUse can't modify output)
- **CLI flags:** https://code.claude.com/docs/en/cli-reference.md (`--append-system-prompt`, `--mcp-config`)
- **Permissions:** https://code.claude.com/docs/en/permissions.md (deny rules + patterns)

### 📝 Next Steps

1. **Design reverse proxy** (Rust or Go; handle mTLS, streaming, retries)
2. **Secret detection** (regex bank: API keys, credentials, AWS secrets, etc.)
3. **Quota exposure** (IPC or file-based state for IDE polling)
4. **Integration test** (launch managed session, verify redaction + quota visibility)
5. **Document for operator** (how to enable, what's redacted, quota enforcement rules)

---

**Memory:** Full details in `.ai/memory/claude-code-integration-facts.md`
