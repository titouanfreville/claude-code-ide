# Claude Code Integration Research — File Guide

**Research Date:** 2026-06-08  
**Status:** ✅ COMPLETE  
**Scope:** API routing, hooks, networking, tool control for MoonlightCode's secret redaction + quota governance

---

## Quick Navigation

### For Immediate Context (5 min read)
👉 **Start here:** [`01-RESEARCH-COMPLETE.md`](01-RESEARCH-COMPLETE.md)
- Key findings summary
- Hard blockers identified
- Architecture diagram
- Next steps

### For Detailed Reference (bookmark these)

1. **Full Integration Facts** (11 KB)
   - File: `.ai/memory/claude-code-integration-facts.md`
   - Contains: All technical details, contracts, gotchas
   - Use: When you need specifics on hooks, flags, proxies

2. **Reverse Proxy Implementation** (9 KB)
   - File: `reverse-proxy-implementation-guide.md`
   - Contains: Code sketches, redaction patterns, testing strategy
   - Use: When ready to design the proxy

3. **Quick Findings** (4 KB)
   - File: `claude-code-api-research-findings.md`
   - Contains: Summary table, checklist
   - Use: For handoff to next session

---

## The Five Questions You Asked

| # | Question | Answer |
|---|----------|--------|
| 1 | Does CC respect `ANTHROPIC_BASE_URL`? | ✅ YES — all traffic (system, messages, tools, files) |
| 2 | Can hooks modify content? | ❌ NO — PostToolUse fires too late; PreToolUse can't rewrite |
| 3 | Where are rate-limits visible? | ❌ NOT in CC — only at reverse proxy (HTTP response headers) |
| 4 | Do `--append-system-prompt` + `--mcp-config` exist? | ✅ YES — both work, compose safely, per-session only |
| 5 | Can we steer toward MCP tools? | ⚠️ PARTIAL — deny built-in + CLAUDE.md guidance (soft only) |

---

## The Three Hard Blockers

### 1. Secret Redaction
**Problem:** Secrets in file reads reach the API  
**Root cause:** PostToolUse hooks fire AFTER output is captured  
**Solution:** Reverse proxy intercepts request JSON before forward

### 2. Rate-Limit Visibility
**Problem:** CC has no access to rate-limit headers  
**Root cause:** No hook exposure; no statusline API  
**Solution:** Reverse proxy captures headers, exposes locally (file/IPC)

### 3. Tool Steering
**Problem:** Can't prefer MCP tools over built-ins  
**Root cause:** CC model doesn't know tool origin; no preference mechanism  
**Solution:** Deny built-in tools + CLAUDE.md guidance (enforcement + soft guidance)

---

## The Architecture

```
Claude Code Session
  ├─ env ANTHROPIC_BASE_URL=http://127.0.0.1:9999
  └─ env HTTPS_PROXY=http://127.0.0.1:9999
       ↓
Reverse Proxy (YOUR CODE)
  ├─ Parse request JSON
  ├─ Apply redaction (regex: API keys, passwords, AWS secrets, etc.)
  ├─ Forward to api.anthropic.com
  ├─ Capture response headers (RateLimit-*, usage, etc.)
  ├─ Write quota state (~/.moonlight/cc-quota.json)
  └─ Return response to CC

IDE (Your App)
  └─ Polls quota state file (5-10s interval)
      ├─ Displays rate-limit headroom
      ├─ Queues/delays sessions if needed
      └─ Logs redaction events to audit table
```

**Why this works:**
- Catches secrets BEFORE they leave the machine
- Access to ALL traffic (not just tool output)
- Access to rate-limit headers
- Can enforce quota client-side

---

## What's Already Wired in MoonlightCode

✅ MCP actor (Slice 3 done)  
✅ PDP gate (permission + danger classification)  
✅ Approval holds (KeystoneApprovalGate)  
✅ Audit logging (moonlight.db)  
✅ Phase transitions (Discovery → Plan → Auto → Test → Review → Commit)  
✅ Session management (resume, reset, registry)  
✅ CLI flag injection (`--append-system-prompt`, `--mcp-config`)

**Missing (proxy layer):**
❌ Secret redaction  
❌ Rate-limit visibility  
❌ Quota enforcement

---

## Next Session Preparation

### To Continue From Here
1. Read `01-RESEARCH-COMPLETE.md` (5 min)
2. Decide: Rust or Go for the proxy?
3. Pick a name: cc-proxy, cc-gateway, etc.
4. Estimate effort: MVP = 2-3 days (forward + redact + quota state)

### Files to Reference
- **When designing the proxy:** `reverse-proxy-implementation-guide.md`
- **When implementing hooks:** `.ai/memory/claude-code-integration-facts.md` (section 2)
- **When integrating with IDE:** Same memory file (section 3 + 7)

### Questions to Ask Operator
- [ ] Rust or Go for the proxy?
- [ ] Which redaction patterns are critical? (AWS, OpenAI, Anthropic, generic Bearer, passwords, etc.)
- [ ] Where should quota state live? (file, IPC, database, metrics endpoint?)
- [ ] What quota thresholds trigger warnings/queuing? (80% / 95% / custom?)

---

## Sources

All findings from official Claude Code documentation:
- [network-config.md](https://code.claude.com/docs/en/network-config.md) — ANTHROPIC_BASE_URL, proxies, CA, mTLS
- [hooks.md](https://code.claude.com/docs/en/hooks.md) — event types, JSON contracts, lifecycles
- [cli-reference.md](https://code.claude.com/docs/en/cli-reference.md) — all flags including `--append-system-prompt`, `--mcp-config`
- [permissions.md](https://code.claude.com/docs/en/permissions.md) — deny rules, tool-specific patterns
- [tools-reference.md](https://code.claude.com/docs/en/tools-reference.md) — complete tool list, availability

No assumptions. Everything cited.

---

## Completion Stats

- **Research time:** ~1 hour
- **Documentation:** 4 files (28 KB total)
- **Official sources consulted:** 5 pages
- **Questions answered:** 5 (3 "yes", 1 "no", 1 "partial")
- **Hard blockers identified:** 3 (all have workarounds)
- **Deliverables:** Implementation guide + detailed reference + architecture diagram

---

**Ready for:** Design + implementation of reverse proxy layer  
**Status:** ✅ Research complete · Architecture clear · Next steps defined
