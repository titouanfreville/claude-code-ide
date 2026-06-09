# Claude Code Integration Research — COMPLETE

**Completed:** 2026-06-08 · **Scope:** API routing, hooks, networking for MoonlightCode secret redaction + quota governance

---

## What Was Asked

Build two capabilities for MoonlightCode:
1. **Secret redaction firewall** — prevent API keys/credentials from reaching Anthropic's API
2. **Token/context governor** — track quota and enforce limits (queue sessions, delay launches)

With precise answers on:
- Does CC respect `ANTHROPIC_BASE_URL` + proxy env vars?
- Can hooks modify content before the API call?
- Where can we intercept rate-limits + token usage?
- What's the exact JSON contract for hooks?
- Do `--append-system-prompt` and `--mcp-config` exist + work as expected?
- Can we steer the model toward MCP tools over built-ins?

---

## Key Findings

### ✅ The Good News
- **All CC traffic routes through `ANTHROPIC_BASE_URL`** — system, messages, tools, everything
- **Both `HTTPS_PROXY` and `HTTP_PROXY` work** — standard environment variables
- **`--append-system-prompt` and `--mcp-config` exist and compose safely** — per-session injection without mutating CLAUDE.md
- **Permission deny rules work** — can lock down sensitive paths
- **MCP actor + PDP gate already wired** — phase transitions, verb auditing, approval holds

### ❌ The Hard Blockers

| Problem | Root Cause | Workaround |
|---------|-----------|-----------|
| **Secret redaction** | PostToolUse hooks fire AFTER output is captured | Reverse proxy (intercept request JSON before forward) |
| **Rate-limit visibility** | No hook exposure; no statusline API | Reverse proxy (parse response headers, expose locally) |
| **Tool steering** | CC model doesn't know tool origin; no preference mechanism | Deny built-in + CLAUDE.md guidance (soft enforcement only) |
| **Tool input rewriting** | PreToolUse hooks only allow/deny, no modification | Reverse proxy or deny + feedback |

### 📌 The Architecture Implication

**Your reverse proxy is the trust boundary.**

```
CC Session (env ANTHROPIC_BASE_URL, HTTPS_PROXY)
    ↓ (all traffic)
Reverse Proxy (YOUR CODE)
    ├─ Parse request JSON
    ├─ Redact secrets (regex, entropy, patterns)
    ├─ Forward to api.anthropic.com
    ├─ Parse response headers
    ├─ Expose quota state (file or IPC)
    └─ Continue session
```

This is the ONLY way to:
- Catch secrets before they leave the machine
- See ALL model inputs (not just tool outputs)
- Access rate-limit headers
- Enforce quota client-side

---

## Deliverables Created

### 1. **Research Summary** (this file)
Quick reference of findings and architecture.

### 2. **Detailed Memory** (`.ai/memory/claude-code-integration-facts.md`)
11 KB reference document covering:
- API routing & networking (ANTHROPIC_BASE_URL, proxies, CA, mTLS)
- Hook event contracts + JSON shapes
- Rate-limit visibility (not exposed; proxy is the answer)
- CLI flags (--append-system-prompt, --mcp-config)
- Tool steering (what's possible, what isn't)
- Secret redaction architecture (reverse proxy design)

### 3. **Handoff Summary** (`.ai/handoffs/claude-code-api-research-findings.md`)
3.6 KB checklist + findings for next session.

### 4. **Implementation Guide** (`.ai/handoffs/reverse-proxy-implementation-guide.md`)
Detailed guide with:
- Redaction patterns (AWS keys, API tokens, passwords, etc.)
- Request/response flow
- Code sketches (Rust + Go)
- Integration with MoonlightCode (env vars, quota polling, audit)
- Testing strategy + rollout checklist

---

## Next Steps for MoonlightCode

### Phase 1: Reverse Proxy MVP
- [ ] Decide on language (Rust preferred for consistency with MoonlightCode)
- [ ] Implement basic proxy (forward requests, capture headers)
- [ ] Add redaction engine (regex patterns for secrets)
- [ ] Write quota state file (`~/.moonlight/cc-quota.json`)
- [ ] Test end-to-end with a managed session

### Phase 2: IDE Integration
- [ ] IDE reads quota state file (5-10s poll interval)
- [ ] Display rate-limit headroom in status bar or Services panel
- [ ] Implement session queueing (if remaining requests < threshold, queue new launches)
- [ ] Log redaction events to audit table

### Phase 3: Operator Feedback
- [ ] Allow operator to configure:
  - Which patterns to redact
  - Quota thresholds (warn at 80%, error at 95%)
  - Redaction log retention
- [ ] Dashboard showing redaction statistics + quota trends

---

## Official Documentation References

All findings verified against official CC docs:
- **Network config:** https://code.claude.com/docs/en/network-config.md
- **Hooks:** https://code.claude.com/docs/en/hooks.md
- **CLI reference:** https://code.claude.com/docs/en/cli-reference.md
- **Permissions:** https://code.claude.com/docs/en/permissions.md
- **Tools reference:** https://code.claude.com/docs/en/tools-reference.md

No guesses; everything cited.

---

## For the Operator

**TL;DR:**
- CC will route all traffic through a proxy you control (via `ANTHROPIC_BASE_URL`)
- Hooks can't redact output, but your proxy can redact the entire request
- Rate-limit headers are visible at the proxy, not to CC
- You already have MCP, phase gates, and audit logging; add the proxy layer and you have complete control

**Next conversation:** Design + spec the reverse proxy, then build it.

---

**Research completion:** 2026-06-08 18:00 UTC  
**Session:** claude-code-guide (a24df0814f6ea4bad)  
**Files:** 4 deliverables · 1 memory update
