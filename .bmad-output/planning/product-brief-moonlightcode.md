---
title: "Product Brief: MoonlightCode"
status: "draft"
created: "2026-06-01"
updated: "2026-06-01"
inputs:
  - .bmad-output/brainstorming/brainstorming-session-2026-06-01-191535.md
---

# Product Brief: MoonlightCode

> Working title — easy to rename. An AI-centric development cockpit for orchestrating many Claude Code sessions.

## Executive Summary

Developers who lean heavily on Claude Code don't write much code anymore — they *run agents that write code*. But the tools for steering those agents haven't caught up. You end up juggling many parallel Claude Code sessions across terminals and tabs, with session monitors like CodeIsland that can *show* you state but never let you *act* on it. The result is constant context-switching: hunting for which session is blocked, which finished, what changed, and whether it's safe to commit.

**MoonlightCode** is a JetBrains-shell IDE built for this new reality — but instead of a code editor in the center, Claude Code sessions sit center-stage. It is not a passive dashboard; it is a **workflow state machine** that knows each session's phase (Plan → Auto-Implement → Test → Review → Commit), auto-reverts a session to **Plan mode** the moment work is declared done, and lets you flip a single session between **Auto** and **Plan** with one key. Crucially, Claude becomes a **first-class actor inside the IDE**: through a built-in MCP server under graduated trust tiers, it can run tests with coverage, query the database, fire HTTP requests, drive the debugger, and submit its own diffs to your review queue — all while you watch from one place. Token economy is native (RTK optimization on every tool's output) and an always-on, oh-my-claudecode-style HUD keeps context %, token burn, and rate-limit headroom in view.

The goal is simple and personal: **never miss a session that needs you, spend fewer tokens, and act on everything from one cockpit instead of watching from the sidelines.**

## The Problem

The person feeling this pain runs Claude Code all day, across many simultaneous sessions, and rarely hand-codes — Claude does that. Today's stack (Claude Code terminals + CodeIsland for state) creates three compounding frictions:

- **You can watch, but you can't act.** CodeIsland surfaces session state, but reviewing a change, approving/rejecting it, re-steering a session, or running a test still means leaving the monitor and switching contexts. The monitor and the place-where-work-happens are different places.
- **Sessions run past the finish line.** There is no phase awareness. A session in "auto" mode barrels past the point where you wanted to inspect the plan, so you lose the natural human gate between *implement* and *commit*.
- **Volume erodes attention.** With many sessions running, it's genuinely hard to know what's running, waiting on you, freshly done, or errored — and a blocked session sitting idle is wasted wall-clock and wasted tokens.

The cost of the status quo is real: idle blocked sessions, redundant context-switching, missed reviews, and uncontrolled token spend.

## The Solution

MoonlightCode is a single cockpit that closes the loop between *watching* and *doing*:

- **Orchestration.** A live grid of session tiles (resizable, with a focus mode), a prioritized **"needs you" queue**, session pinning, and traffic-light status (badge emoji + colored tile name/border). Attach a repo path to a session; spawn new sessions with a pre-filled prompt, path, and tool set. OS notifications when a session needs input or finishes. A central terminal multiplexes all sessions.
- **Workflow state machine.** Every session shows its phase. On "done," it **auto-reverts to Plan** so nothing commits without your gate; a one-key **Auto↔Plan** toggle lets you grant speed when you trust the plan. Phases gate permissions (Plan = read-only; Auto = full trust tier). Your loop — Plan → Auto → manual test → review (bmad + sheik) → manual review → commit — can be encoded as the default template.
- **MCP-as-actor, under trust tiers.** Claude operates autonomously but observably: `run_with_coverage`, `query_db`, `http_request`, `start_debug`, `open_review`. Every autonomous action is audit-logged and one-click revertible. A danger-zone list (e.g. force-push, prod DB) always requires human approval, aligned with Claude Code's default harness.
- **Review surface.** Live per-session diffs (IntelliJ-style), a cross-session review queue, per-hunk accept/reject where rejection becomes feedback injected back to the session, and a "since I last looked" view.
- **RTK-native economy + active governor.** RTK wraps the commands sessions run (its proven model) for per-call savings, *and* the orchestration layer acts as a **fleet-level spend governor**: when rate-limit headroom drops it can pause or down-shift low-trust sessions and route by phase. The HUD shows context %, token burn, model, rate-limit headroom, and a tokens-saved counter. *(Note: RTK works by wrapping known commands; compressing arbitrary MCP/tool output is a separate, unproven mechanism — see Key Risks.)*

- **Continuity.** Sessions survive an IDE restart and each carries a "what was I doing" summary, so a crash never costs you the mental map of N running sessions.

- **Rejection-as-feedback (signature primitive).** Every gate — a rejected hunk, a failed test, a denied danger-zone action — emits *structured corrective feedback back into the session*, not just a state change. This closes the human→agent steering loop that pure monitors lack, and it is the conceptual core, not a side feature.

## What Makes This Different

This is not a session monitor with buttons bolted on. Three things distinguish it:

1. **It's a workflow state machine, not a dashboard.** Phase-aware sessions that auto-revert to Plan and gate permissions by phase is a control model nothing else offers.
2. **Claude is an actor, not a subject.** The built-in MCP server lets Claude drive the IDE's own tools under graduated trust — bidirectional, not just observability.
3. **Token economy is first-class.** RTK optimization and an OMC-style HUD are native, not afterthoughts.

The honest moat is **fit and execution**: it's shaped precisely around one power-user's real loop, and it integrates the exact tools that loop already depends on (Claude Code, RTK, oh-my-claudecode, bmad, sheik).

## Who This Serves

- **Primary — you (the builder).** A heavy Claude Code user who orchestrates many parallel agent sessions and rarely hand-codes. Success looks like: never missing a blocked session, lower token spend, and acting on everything from one place.
- **Secondary — eventually, a small circle of friends/coworkers.** Once the tool feels good for you, it may be shared. This is a *possible future*, not a current goal — no ecosystem, marketplace, or multi-tenant concerns in scope now.

## Success Criteria

- **Zero missed blockers** — you always know, at a glance or via notification, which sessions are waiting on you; idle-blocked time trends toward zero. *(Primary signal.)*
- **Lower token spend** — measurable reduction in token/cost burn per session vs. today, surfaced live by the RTK-native HUD. *(Primary signal.)*
- **Act-from-one-place** — reviewing, approving/rejecting, and re-steering happen inside MoonlightCode without leaving for terminals/tabs (the "can only watch" pain is gone).
- **Phase discipline** — sessions reliably stop at Plan on "done"; nothing reaches commit without your gate.
- **Comfortable concurrency** — you can steer more parallel sessions than today without losing track. *(Tension to manage: the manual review gate makes the human the throughput ceiling; mitigations include batched cross-session review and auto-approving small, safe diffs.)*

> **Baseline first.** Before building, instrument the current state for one week — how many sessions run in parallel, how often a blocker sits idle and for how long, and today's token/cost burn. Without this baseline, "lower token spend" and "comfortable concurrency" can't be proven, only felt.

## Scope

**In (Lean-core MVP — the JetBrains-shell IDE):**
1. Session orchestration (tiles + focus, needs-you queue, pinning, traffic-light, attach-path, spawn, central terminal, OS notifications)
2. Workflow state machine (phase per session, auto-revert to Plan, Auto↔Plan toggle, phase-gated permissions, encode-your-loop template) — with **rejection-as-feedback** as the steering primitive
3. MCP-as-actor with basic trust tiers + audit/revert + danger-zone approvals (thin v1 of each verb)
4. Review surface (live diff, cross-session "since I last looked" feed, per-hunk accept/reject → feedback)
5. RTK-native token economy + **active fleet governor** + OMC-style HUD basics
6. Lightweight continuity (sessions survive restart + "what was I doing" summaries)

**Phase 2 (next):** HTTP collections / Run-Debug configs / DB explorer (all in-repo & GitHub-shareable); debugger depth (timeline, rewind, error spotlight, hallucination detector); shared cross-session memory/wiki.

**Explicitly out (for now):** Ecosystem / plugin marketplace / multi-user, full theming engine, Analyst stats suite, and black-swan ideas (adversarial plan-debate, living repo wiki). Mobile companion, watch/pair mode.

**Technical direction (high-level):** Rust-leaning stack. **No Java.** Native desktop shell (e.g. Tauri-class) hosting terminals + dockable panels; a local MCP server exposing the IDE's tools to Claude; first-class integration of RTK and oh-my-claudecode.

## Key Risks & Open Questions

These are the load-bearing unknowns the brief depends on. They should be spiked before committing to the full build:

1. **Control vs. observe (the linchpin).** The entire "act, don't just watch" thesis assumes MoonlightCode can *control* Claude Code — force Plan mode, enforce phase-gated permissions, inject the MCP actor — not merely read its state. Claude Code owns its own permission/harness model. **Open question: what's the actual control surface?** (Hooks like `Notification`/`Stop`/`PreToolUse`, the Agent SDK, MCP, or a wrapper — vs. needing a fork.)
2. **"Done"/"blocked"/"phase" detection.** Auto-revert-to-Plan and the needs-you queue hinge on reliably detecting these signals across sessions. **What concrete signal marks each?** (Claude's output text, a hook event, an exit code, session JSONL state.) This is the first thing to spike.
3. **MCP-actor verbs are subsystems.** `start_debug`, `query_db`, `run_with_coverage`, etc. are each substantial — a debugger integration alone is multi-month. The MVP must define how thin the v1 version of each is, or defer them.
4. **RTK over arbitrary output.** RTK's proven model wraps known commands; "transparent on all MCP/IDE output" is a different, unvalidated mechanism.
5. **Solo build feasibility.** A JetBrains-class Tauri shell (docking, terminal host, IntelliJ-grade diffs) is months of UI plumbing that delivers little of the #1 value early. Strongly consider a staged build (see Roadmap) and composing existing blocks — Claude Code session JSONL under `~/.claude/projects/`, Claude Code hooks, tmux, RTK as a CLI wrapper, oh-my-claudecode state tools — rather than building from scratch.

## Roadmap Thinking (build order within the IDE)

The MVP is the JetBrains-shell IDE (decided). To de-risk a build of that size, sequence it so the load-bearing unknowns are proven *before* heavy UI investment, and so value lands on the first vertical slice:

- **Spike 0 — De-risk first (non-negotiable gate):** Prove the **control surface** (Risk #1 — can we force Plan, gate permissions, inject the MCP actor via hooks / Agent SDK / MCP, or do we need a fork?) and **done/blocked/phase detection** (Risk #2 — what concrete signal marks each, from session JSONL + hooks). Throwaway prototype, no UI polish. Nothing else proceeds until this is answered.
- **MVP slice 1 — Watch & act:** Inside the IDE shell, ship the orchestration core first — needs-you queue, traffic-light tiles, OS notifications, attach-to-session — so the #1 value (never miss a blocker, act from one place) lands earliest.
- **MVP slice 2 — Workflow machine + review:** Phase state, auto-revert-to-Plan, Auto↔Plan toggle, the "since I last looked" review feed, and rejection-as-feedback injection.
- **MVP slice 3 — Autonomy + economy:** Trust-tiered MCP-actor (thin verbs), audit/revert, danger-zone approvals, RTK active governor + HUD, lightweight continuity.
- **Phase 2:** Tool surfaces (HTTP/Run/DB), debugger depth, shared cross-session memory/wiki.

## Vision

If it works, MoonlightCode becomes the default surface from which one person directs a *team* of AI coding agents the way a conductor leads an orchestra — every session visible, every action gated by intent, every token accounted for. The phase-aware workflow machine and trust-tiered MCP layer generalize from "my loop" to "any disciplined AI-development loop," making it the natural thing to hand to a teammate when the time comes. The center of the IDE was the code editor for thirty years; here, it's the agents — and the human stays firmly in command.
