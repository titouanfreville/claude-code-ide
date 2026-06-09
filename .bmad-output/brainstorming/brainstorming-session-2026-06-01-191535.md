---
stepsCompleted: [1]
inputDocuments: []
session_topic: 'IDE-like control surface for Claude Code — JetBrains-class IDE that exposes an MCP server so Claude is a first-class actor, with a central terminal hosting Claude Code integration'
session_goals: 'Expand a half-formed product idea into a distinctive, well-scoped concept; generate 100+ ideas before converging'
selected_approach: 'AI-Recommended — progressive divergent flow (stakeholder lens -> analogies -> SCAMPER -> provocation)'
techniques_used: ['Stakeholder Lens (Orchestrator/Reviewer)']
ideas_generated: []
context_file: ''
---

# Brainstorming Session Results

**Facilitator:** Titouan
**Date:** 2026-06-01

## Session Overview

**Topic:** An IDE-like control surface for Claude Code. Not merely a passive dashboard —
a JetBrains-class IDE where **Claude is a first-class actor**: the IDE exposes an **MCP
server** so Claude can drive its tools directly (run/debug, tests w/ coverage, DB queries,
HTTP requests), plus a **central terminal** hosting the Claude Code integration. Goal:
solve "it's hard to follow everything" when running many Claude Code sessions.

**Goals:** Diverge widely first (target 100+ ideas), then converge. Output feeds a Product Brief.

### Context Guidance

- User runs Claude Code heavily; currently uses **CodeIsland** for session state — new tool must go *beyond* it.
- **Tech:** TBD, Rust-leaning. **Hard constraint: no Java.**
- **RTK (Rust Token Killer)** token-optimization is a desired first-class feature of the IDE.

### Priority Personas

`Orchestrator + Reviewer = CORE` · `Debugger = important` · `Analyst = nice-to-have`

## Idea Pool (running)

### Burst 1 — Orchestrator + Reviewer lenses + MCP reframe

Legend: ⭐ must-have · ✅ keep · 🤔 maybe · ✂️ cut/merged

**Orchestrator**
1. ✅ Session grid/tiles (live cards) — *refine: resizable tiles OR focus-mode on select*
2. ⭐ "Needs you" queue — prioritized inbox of sessions blocked on input
3. ✅ Status indicator — **decision: use a badge emoji + apply traffic-light pattern to colorize tile name + border** (merge of orig #3 and #4)
4. ✂️ Merged into #3
5. ✅ Central terminal multiplexing all sessions (session-aware panes)
6. 🤔 Hotkey "jump to session that needs me most" (learnable, possibly valuable)
7. ⭐ Session pinning — keep critical sessions always-visible
8. ⭐ Attach a working dir / repo path to a session; IDE opens that project context
9. ⭐ Spawn-new-session from IDE w/ pre-filled prompt + attached path + chosen MCP tools
10. ⭐ MCP: Claude can spawn/pause/resume sibling sessions itself

**Reviewer** (all kept; needs dedup pass)
11. Live diff stream per session (IntelliJ gutter markers)
12. Cross-session review queue (batch approvals)
13. Per-hunk accept/reject; rejection becomes feedback injected to session
14. "Explain this change" — Claude annotates why (from reasoning)
15. Side-by-side: prompt that caused change ↔ resulting diff (causal traceability)
16. Review checkpoints — freeze session at "ready for review"
17. GitHub-style threaded comments Claude reads back via MCP and acts on
18. "Blast radius" view — files/tests touched before approval
19. Auto-run affected tests on pending diff; inline green/red
20. "Since I last looked" — everything changed across sessions since last glance

### MCP Autonomy Decision

Claude must be able to operate **fully autonomously** but under **high-level observability**:
start app, start debug, run tests **with coverage**, query DB/HTTP and receive results.
Token output must be optimized (**RTK-like; RTK is a must-have layer in this IDE**).

### Additional tool surfaces requested (to mine in later bursts)
- HTTP request manager — Postman-style preferred (JetBrains HTTP client fallback), **easily shareable via GitHub**
- Run/Debug with easy-but-complete setup (JetBrains-quality config UX)
- DB explorer view

### CANONICAL LAYOUT (decided)
JetBrains-style shell, but the **center pane = Claude Code terminals** (not a code editor —
this IDE is **AI-centric**; user rarely hand-codes). Surrounding docks host plugin views:
Code (inspection), file tree, Run, non-Claude terminal, DB, HTTP, etc.

### USER'S CORE WORKFLOW LOOP (drives the product)
`Plan → refine plan until OK → implement (Auto mode) → manual test (HTTP/UI) → review (bmad + sheik skills) → manual review → commit`
Wishes: sessions **auto-revert to Plan mode** when work is done; keep **Auto-mode** speed when asked;
**easy Auto↔Plan toggle**. The IDE is therefore a **workflow state machine**, not just a monitor.

### Burst 2 — Debugger / Analogies / MCP-actor / RTK / Tool-surfaces
**Debugger (all ✅):** 21 event timeline · 22 rewind to checkpoint · 23 error spotlight · 24 breakpoint-on-Claude · 25 replay tool call raw · 26 hallucination detector (Claude's view vs actual file) · 27 cross-session blame · 28 live reasoning+IO console
**Analogies:** 29 ✅ live gauges (but NOT a Grafana look) · 30 ✅ alerts approaching token/session limit · 31 ✂️ ATC strips (not relevant) · 32 ⭐ GO/NO-GO board before risky multi-session run · 33 🤔→optional plugin view: repo heatmap of where sessions are working · 34/35 ✅ refine: flight-recorder replay + incident-mode freeze
**MCP-actor (✅ fully autonomous + high observability):** 36 run_with_coverage · 37 query_db · 38 http_request · 39 start_debug · 40 open_review · 41 capability/trust tiers per session · 42 audit log + one-click revert · 43 dry-run→approve-once→auto
**RTK / token economy (✅):** 44 RTK transparent proxy on all tool output (built-in) · 45 tokens-saved counter (global+session) · 46 token budget + burn-down · 47 expensive-action warning · 48 auto-summarize stale context · 49 per-tool savings → Analyst stats
**Tool surfaces:** 50 ⭐ HTTP collections as in-repo files (GitHub-shareable) · 51 ⭐ run/debug configs committed · 52 ⭐ DB profiles per-project, secrets vaulted · 53 ✅refine sandbox/replica guardrail for Claude-written queries

### Burst 3 — Terminal-first / Memory / Sharing / Ambient / Safety
**Terminal-first:** 54-58 → resolved into CANONICAL LAYOUT above (JetBrains shell, Claude center)
**Memory/continuity:** 59 ⭐ sessions survive restart (own store to restore, beyond `resume`) · 60 ⭐ "what was I doing" summary on return · 61 ✅ cross-session shared memory/wiki · 62 🤔 branch-from-timeline (lower prio) · 63 NTH session templates
**Sharing/team:** 64 ✅ session replay export (shareable) · 65 NTH watch mode (pair) · 66 ⭐ in-repo HTTP/run configs → git-clone onboarding · 67 ✅ auto-draft PR description from session · 68 ✅ session-as-documentation
**Ambient:** 69 ⭐ OS notification on needs-input/finished · 70 NTH menubar/tray HUD · 71 NTH mobile companion · 72 ✅ DND/focus batching · 73 ✅ audio cues
**Safety:** 74 ✅(hard) per-session sandbox/worktree · 75 ✅ danger-zone always-approve list (based on CC default harness) · 76 NTH spending circuit-breaker · 77 🤔 global kill-switch (keep idea, not required now) · 78 ✅ approval delegation (auto-approve small safe diffs)

### Burst 4 — Workflow state-machine / Analyst / Black-swan
**Workflow state-machine (CORE):** 79 explicit phase per session shown on tile · 80 ⭐ auto-revert to Plan on done · 81 ⭐ one-key Auto↔Plan toggle · 82 phase-gated permissions · 83 on-done auto-run tests then bmad+sheik review · 84 review results route back as next plan · 85 plan-diff · 86 per-phase definition-of-done checklist · 87 templated workflow per project ("encode your loop") · 88 commit only after manual review gate
**Analyst (nice-to-have):** 89 most-used tools/skills leaderboard · 90 time-per-phase analytics · 91 token/cost by session/project/phase · 92 skill ROI · 93 plan→commit cycle-time trend · 94 productive/blocked heatmap · 95 tool-failure stats · 96 weekly retro digest
**Black-swan:** 97 NTH reverse mode (AI-centric, user doesn't code) · 98 ⭐ two sessions debate a plan, you pick (adversarial planning) · 99 🤔refine time-travel commit · 100 ⭐ living repo wiki kept updated as sessions change code · 101 ✅ sessions negotiate file locks via MCP (known pain) · 102 ✂️ ghost mode (dropped)

### Burst 5 — Status HUD / Theming / Onboarding / Ecosystem
**Status HUD (OMC-inspired, user likes OMC bar):** 103 global bar (context%, token burn, model, rate-limit headroom) · 104 per-session mini-HUD on tile · 105 context-pressure warning · 106 model badge per session · 107 git state in HUD · 108 cost-today ticker + projected burn · 109 configurable HUD presets · 110 compact/expanded toggle
**Theming/views:** 111 dockable/resizable panels (refines #1) · 112 layout presets (Orchestrator/Review/Debug) · 113 save/switch workspaces · 114 focus mode (full-screen one session) · 115 theme engine + accent drives traffic-light palette · 116 density toggle · 117 plugin-driven dockable views (heatmap/DB/HTTP) · 118 pin-vs-summon panels
**Onboarding:** 119 zero-config first-run (detect Claude Code/RTK/OMC, offer import) · 120 import CodeIsland/CC history day one · 121 "encode your loop" wizard · 122 project detector auto-suggests configs · 123 guided first session · 124 sane defaults from CLAUDE.md/RTK/OMC · 125 "what can Claude do here" MCP+trust panel
**Ecosystem:** 126 plugin API for views · 127 skill marketplace (bmad/sheik/OMC) in-IDE · 128 RTK & OMC bundled first-class · 129 MCP server registry per trust tier

---

## Convergence & Synthesis

### Concept (one line)
An **AI-centric, JetBrains-shell IDE** where Claude Code sessions live in the center, the IDE
is a **workflow state machine** (Plan→Auto→Test→Review→Commit) that Claude drives via an **MCP
server under graduated trust**, with **RTK token-optimization** and **orchestration of many
parallel sessions** as the core experience.

### Distinctive pillars (the "why not CodeIsland/JetBrains")
1. **Workflow state machine** — phase-aware sessions, auto-revert to Plan on done, one-key Auto↔Plan, phase-gated permissions.
2. **MCP-as-actor under trust tiers** — Claude autonomously runs/debugs/tests/queries; user watches & gates.
3. **RTK-native + OMC-style HUD** — token economy is first-class.

### 10 Pillars
1. Session Orchestration · 2. Workflow State Machine (differentiator) · 3. MCP-as-Actor + Autonomy/Safety ·
4. Review surface · 5. Token economy / RTK-native · 6. Debugger · 7. Tool surfaces (in-repo, GitHub-shareable) ·
8. Memory/continuity · 9. Sharing/Onboarding/Ecosystem · 10. Views/Theming/HUD

### MVP DECISION — "Lean core"
- **🟢 MVP:** Pillars 1 (Orchestration) + 2 (Workflow State Machine) + 3 (MCP autonomy/safety, basic trust) + 4 (Review queue + accept/reject) + 5 (RTK + HUD basics).
- **🟡 Phase 2:** Pillar 7 (HTTP/Run/DB tool surfaces), 6 (Debugger depth), 8 (Memory/continuity).
- **🔵 Later:** 9 (ecosystem/plugin API/marketplace), 10 (full theming), Analyst stats (89–96), black-swans (98 plan-debate, 100 living wiki).

### Tech constraints carried forward
- Rust-leaning stack, **no Java**. RTK + oh-my-claudecode integrated first-class. Center = Claude (user rarely hand-codes).

### Next step
→ **BMad Product Brief** (bmad-product-brief skill) using this synthesis as the spine.
