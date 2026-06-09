---
title: "Spike 0 Findings — Claude Code Control Surface & Detection"
status: "complete"
date: "2026-06-02"
verdict: "GO — L0 full, L2 substantial (via own PDP), L1 partial"
relates_to: ".bmad-output/planning/architecture.md (Risk #1 / ControlPort)"
---

# Spike 0 — Control Surface & Detection: Findings

**Question:** Can MoonlightCode *control* Claude Code (force Plan, gate/deny permissions, inject feedback) and *detect* done/blocked/phase — not merely observe? This gates the whole build.

**Verdict: GO.** The control surface is real, documented, stable, and already exploited in the wild (CodeIsland uses the same hooks). Capability levels achieved:
- **L0 Observe — FULLY achievable.**
- **L1 Inject feedback — PARTIAL (sufficient for rejection-as-feedback).**
- **L2 Govern (gate/deny + plan) — SUBSTANTIAL, with one caveat (below).**

## Evidence

### Local environment (this machine)
- **CodeIsland already hooks** `Notification`, `PermissionRequest`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PreCompact`, `SessionEnd` (from `~/.claude/settings.json`) — proof the detection + interception surface works in production today.
- **388 session JSONL files** under `~/.claude/projects/` — rich, real detection corpus.
- **Transcript JSONL schema (observed, not assumed)** — per-line `type` values include:
  `attachment, assistant, user, system, mode, permission-mode, last-prompt, ai-title, file-history-snapshot, queue-operation`.
  - **`permission-mode` events** (`{permissionMode, sessionId, type}`) are written as **discrete transcript lines** → current mode (plan/default/acceptEdits/…) is **directly observable by tailing JSONL**. Strong signal for phase detection.
  - **`mode` events** likewise track mode changes.
  - **`ai-title`** → Claude already generates a session title (free label for the grid UI).
  - **`system` events** carry `stopReason, hookCount, hookErrors, preventedContinuation, toolUseID` → rich lifecycle/diagnostic signal.
  - **`queue-operation`** → prompt queueing is visible.
  - Append-only, tail-able in real time.

### Authoritative capabilities (Claude Code docs, 2026)
| Capability | Result | Mechanism |
|---|---|---|
| Block/deny a tool call | **YES** | `PreToolUse` hook → `permissionDecision: "deny"` (+ `permissionDecisionReason`), or exit code 2. Hooks run *before* permission rules; deny takes precedence. |
| Auto-approve/deny permission prompts | **YES** | `PermissionRequest` hook → `decision.behavior: "allow"\|"deny"`, optional `updatedInput`. |
| Inject feedback Claude acts on | **PARTIAL** | `additionalContext` (PostToolUse, UserPromptSubmit), `permissionDecisionReason` on denial. NOT arbitrary mid-reasoning — injection happens at turn/tool/denial boundaries. |
| Force Plan mode at spawn | **YES** | `claude --permission-mode plan` / SDK `permissionMode: "plan"`. |
| Force Plan on a *running* session externally | **NO** | No external toggle; only the user (Shift+Tab) can flip a live session. ← the one caveat. |
| Programmatic spawn / drive / stream / resume | **YES** | Agent SDK (TS/Python): `query()`, `permissionMode`, `allowedTools`, `hooks`, `resume`, `includePartialMessages` streaming; CLI `claude -p --output-format stream-json`. |
| Per-tool permission callback | **YES** | PreToolUse / PermissionRequest hooks (SDK callback form). |
| Detect blocked / idle / working / done | **YES** | `PermissionRequest`=blocked; `UserPromptSubmit`(empty return)=idle/awaiting; `Stop`/`SessionEnd`=done; `Pre/PostToolUse`=working. Plus JSONL tail. |
| Stable hook payload schema | **YES** | Documented: `session_id, hook_event_name, cwd, permission_mode, transcript_path, tool_name, tool_input, tool_use_id`. |

## Key Architectural Consequence

**The one caveat — can't flip a *running* session to Plan externally — does NOT block FR13/FR14.** It reshapes *how* they're implemented:

- MoonlightCode does **not** rely on toggling Claude's internal mode switch. Instead, **the PDP (already in the architecture) owns enforcement** by denying tool calls that aren't allowed in the current phase, via `PreToolUse → deny` + `permissionDecisionReason`. This *is* the single-authority model we designed (decision E) — Spike 0 validates it.
- **FR13 (auto-revert-to-Plan on done):** on detecting "done," the supervisor sets the session's phase to Plan; the PDP then denies edit/write/commit tools (read-only) and the next turn can be **resumed with `permissionMode: plan`** via the SDK. Functionally equivalent to a native flip.
- **FR14 (Auto↔Plan toggle):** implemented as "what the PDP allows for the next/just-gated actions" + resuming the next turn with the chosen `permissionMode`. Native plan-at-spawn is a bonus, not the dependency.
- **Rejection-as-feedback (FR18–19):** delivered via `permissionDecisionReason` (on deny) and `additionalContext` (on next turn) — exactly the L1 surface, sufficient for the signature primitive.

## The fork Spike 0 surfaces (needs a decision)

There are **two integration postures**, and they imply different day-to-day UX:

- **Path A — Global hooks + JSONL tail (attach to sessions started anywhere).** Register global hooks like CodeIsland; observe + gate (PreToolUse deny) sessions the user launches however they like (incl. the native CC TUI). Lower control over spawn/lifecycle; "phase" enforced purely via the PDP's deny layer; can't pre-set plan mode for sessions you didn't spawn.
- **Path B — Agent SDK owns sessions (spawn/drive programmatically).** MoonlightCode spawns each session via the Agent SDK: native streaming events, per-tool callbacks, `permissionMode`, `resume`, interrupt. Full L2 per session. Sessions are SDK-driven (MoonlightCode provides the terminal/UI anyway). Sessions started *outside* MoonlightCode aren't governed unless also caught by global hooks.
- **Path C — Hybrid (recommended):** SDK-owned sessions for full governance (the primary cockpit experience) **+** global hooks/JSONL as a secondary "observe & gate" net for sessions started elsewhere.

The architecture's `ControlPort` abstraction already accommodates all three — this only decides which adapter is primary.

## Bottom line
Risk #1 is **retired**. The build is GO at **full-MVP** ambition (not degraded-MVP): gating, denial-as-feedback, and plan-at-spawn are all real; the only limitation (no live external mode-flip) is absorbed cleanly by the PDP design. Detection is *better* than assumed — the transcript even logs explicit `permission-mode` events.
