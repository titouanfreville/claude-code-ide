---
stepsCompleted: [1, 2, 3, 4, 5, 6, 7, 8]
lastStep: 8
status: 'complete'
completedAt: '2026-06-02'
inputDocuments:
  - .bmad-output/planning/prd.md
  - .bmad-output/planning/product-brief-moonlightcode.md
  - .bmad-output/brainstorming/brainstorming-session-2026-06-01-191535.md
workflowType: 'architecture'
project_name: 'MoonlightCode'
user_name: 'Titouan'
date: '2026-06-01'
---

# Architecture Decision Document

_This document builds collaboratively through step-by-step discovery. Sections are appended as we work through each architectural decision together._

## Project Context Analysis

### Requirements Overview

**Functional Requirements (52 FRs, 11 capability areas):** The capabilities cluster into architectural responsibilities:
- **Orchestration & Awareness (FR1–11):** session supervisor managing N external Claude Code processes + PTYs; live grid/queue UI fed by a detection layer.
- **Workflow & Phase Control (FR12–17):** a per-session **phase state machine** (Plan→Auto→Test→Review→Commit) with auto-revert and phase-gated permission enforcement.
- **Steering & Feedback (FR18–20):** a feedback-injection channel back into a session (the signature primitive) — requires the *write* side of the control surface.
- **Review (FR21–24):** diff capture + cross-session review feed + per-hunk decisions.
- **Agent Autonomy / MCP-Actor (FR25–31, FR52):** an **embedded local MCP server** exposing actor verbs, gated by trust tiers + lock negotiation.
- **Safety & Audit (FR32–36, FR51):** approval interception, append-only audit log, one-click revert, GO/NO-GO launch board, sandbox isolation.
- **Token Economy & Observability (FR37–41):** RTK wrapping, HUD telemetry, fleet governor.
- **Continuity (FR42–44):** local persistence + restart recovery + session summaries.
- **Baseline & Setup (FR45–46):** usage instrumentation + environment auto-detection.

**Non-Functional Requirements (20 NFRs):** The drivers that shape design most:
- **Performance/real-time:** ≤1s state propagation, <100ms UI, ≥10 concurrent sessions, governor must never block UI (NFR1–4) → event-driven core, async runtime, UI decoupled from engine.
- **Reliability:** ≥95% detection accuracy, tolerate malformed Claude Code artifacts, crash-survivable state, graceful degradation if control unavailable (NFR5–8) → defensive parsing, durable state store, fallback mode.
- **Security/Privacy:** keychain secrets, default-deny autonomy, non-overridable danger-zone, zero telemetry, revertible audit (NFR9–13) → a permission-enforcement layer + local-only data.
- **Integration:** no-fork Claude Code integration, optional RTK/OMC, macOS-first/Linux-portable, offline-capable (NFR14–17).
- **Usability:** calm-by-default, keyboard-first (NFR18–20).

### Scale & Complexity
- **Primary domain:** native desktop developer tool / multi-process orchestrator (event-driven, real-time, local-first).
- **Complexity level:** High — driven by *technical novelty* (control surface, real-time multi-session detection, autonomous MCP actor under trust tiers), not data/domain volume.
- **Estimated architectural components (high-level):** ~8 — Desktop Shell (UI), Core Engine/Supervisor, Phase State Machine, Detection Layer, Control-Surface Port (+ adapters), Embedded MCP Server, Trust/Safety/Audit subsystem, Token-Economy/HUD subsystem, Persistence — plus the Claude Code integration boundary.

### Technical Constraints & Dependencies
- **Hard constraints:** Rust-leaning, **no Java**, Tauri-class desktop shell, macOS-first (Linux-portable), local-first with **zero telemetry**.
- **External dependencies:** Claude Code (control + detection — *the* dependency, gated by Spike 0); RTK (command wrapper, optional); oh-my-claudecode (state/HUD data, optional); OS keychain; git (worktrees/sandbox).
- **The gating dependency:** Spike 0 determines the control-surface mechanism (hooks `Notification`/`Stop`/`PreToolUse` / Agent SDK / MCP / session JSONL under `~/.claude/projects/`); the architecture isolates this behind a port so the choice is swappable and degradable.

### Cross-Cutting Concerns Identified
- **Event bus** (phase transitions + detection events → UI, hooks, audit) — the backbone enabling ≤1s propagation and decoupling.
- **Permission/trust enforcement** spanning phases, MCP verbs, and danger-zone — a single authority, not scattered checks.
- **Audit + revert** threaded through every autonomous action.
- **Resilience/defensive parsing** of all Claude Code artifacts (untrusted input).
- **Structured logging/telemetry** (injected, à la the user's Zap pattern) feeding both diagnostics and the HUD.
- **Local persistence** underpinning continuity, audit, and baseline metrics.

## Foundation & Technology Stack

### Primary Technology Domain
Native desktop developer tool — a **single-language, single-build Rust application** (no web frontend, no back/front split). This is a true desktop IDE, not a Tauri-style Rust-core + web-UI hybrid. Rules out Tauri/Electron by design constraint.

### GUI Framework Decision: GPUI

**Selected: [GPUI](https://www.gpui.rs/)** — the GPU-accelerated, hybrid immediate/retained-mode Rust UI framework created by Zed Industries.

**Options considered:**
- **GPUI (selected):** Purpose-built for IDEs (powers Zed, which reached 1.0 in April 2026). Single-language Rust, single `cargo build`, GPU-accelerated — the right fit for a real IDE shell with dockable panels, terminal, and rich diff views. Shares lineage with the Agent Client Protocol ecosystem.
- **egui (fallback):** Mature/stable immediate-mode GUI; chosen as the conservative fallback if GPUI's pre-1.0 instability proves too costly for a solo builder. Heavy IDE widgets (docking, rich diff) require more custom work.
- **Iced / Dioxus / Slint (rejected for this use):** Cleaner in places but weaker IDE-widget ecosystems and/or web/embedded orientation.

**Rationale:** GPUI is the only mainstream pure-Rust option *designed for building an IDE*, satisfying the single-project/single-build constraint while delivering native GPU-accelerated performance (supports NFR2's ≥10-session responsiveness target).

**Key risk (documented):** GPUI is **pre-1.0** — smaller docs/community, faster-moving API, primarily battle-tested inside Zed's own repo rather than as a standalone dependency. **Mitigation:** isolate GPUI behind the app's own view/widget abstractions so a fallback to egui is possible without rewriting the engine; the core engine (below) is fully GUI-agnostic.

### Foundation Stack (verified June 2026)
- **Language/runtime:** Rust (stable channel, ≥1.77).
- **GUI:** GPUI (pre-1.0, pinned git revision or vendored).
- **Async runtime:** `tokio` — the event-driven core, session supervision, MCP server, and detection layer are all async.
- **Terminal/PTY:** [`portable-pty`](https://crates.io/crates/portable-pty) (WezTerm lineage) for spawning/attaching Claude Code session PTYs — Tauri's sidecar model is N/A here; we manage processes directly.
- **Persistence:** SQLite via `sqlx` or `rusqlite` (embedded, local-first) for continuity, audit log, baseline metrics, trust config.
- **MCP server:** the official Rust MCP SDK (`rmcp`) for the embedded actor-verb server.
- **Errors / logging / config:** `thiserror` (domain sentinel errors), `tracing` (injected structured logging → diagnostics + HUD), `figment`/`config` (YAML aggregation) — the Rust analogues of the user's Go conventions (uber/config, Zap, sentinel errors).
- **Secrets:** OS keychain via `keyring`.

### Project Layout — Cargo Workspace (mirrors the user's Go monorepo conventions)
A single Cargo workspace (monorepo of crates), translating the user's Go hexagonal/domain-first layout (copro-manager / prorizon) into idiomatic Rust:

```
moonlightcode/                      # one repo, one `cargo build`
├── Cargo.toml                      # workspace root
├── crates/
│   ├── domain/                     # entities + PORTS (traits) + sentinel errors; no I/O
│   │   └── src/{session, phase, review, trust, audit, economy}/   # domain-first modules
│   ├── engine/                     # core orchestrator: supervisor, phase state machine, event bus, fleet governor
│   ├── detection/                  # adapter: tails ~/.claude/projects/*.jsonl + consumes CC hooks (control-surface read side)
│   ├── control/                    # adapter: the CONTROL-SURFACE port impls (Spike-0-chosen), write side
│   ├── mcp-server/                 # adapter: embedded local MCP server exposing actor verbs
│   ├── persistence/                # adapter: SQLite store (continuity, audit, baseline)
│   └── core/                       # cross-cutting: config, logging/tracing, error helpers, rest-like utils
└── apps/
    └── desktop/                    # GPUI app = composition root (wires ports→adapters), views/panels, HUD
```

**Conventions carried from the Go projects:**
- **Hexagonal / ports-and-adapters:** `domain` holds entities + traits (ports) + sentinel errors and depends on nothing; `*-adapter` crates implement those traits; `apps/desktop` is the single composition root (the Rust analogue of `bin/app/app.go`).
- **Domain-first modules**, not layer-first (`domain/session/`, `domain/review/`… — never `handlers/services/repos`).
- **DI:** no FX equivalent in Rust → constructor injection of trait objects, wired once in the composition root.
- **Config aggregation** (YAML, no env-var expansion by default), **injected structured logging** (`tracing`, named spans), **sentinel domain errors** (`thiserror`).

### Initialization (first implementation story)
Scaffold the Cargo workspace manually (no `create-tauri-app` — Tauri is not used). First story: `cargo new` workspace + the crate skeleton above, a GPUI "hello window", and a `tokio` engine stub. **Note:** the very first *engineering* effort is **Spike 0** (control-surface + detection prototype, throwaway) — it precedes committing to this layout, per the PRD gate.

**ACP note:** Per decision, Spike 0 evaluates only Claude Code's own mechanisms (hooks `Notification`/`Stop`/`PreToolUse`, Agent SDK, MCP, session JSONL). Zed's Agent Client Protocol is explicitly out of Spike 0 scope (revisit only if Claude Code natively adopts ACP).

## Core Architectural Decisions

### Decision Priority Analysis
- **Critical (block implementation):** Control-Surface Architecture (A), Detection Strategy (B), Concurrency & Process Model (C), Trust & Permission Enforcement (E). These are gated by or feed Spike 0.
- **Important (shape architecture):** Persistence & Audit (D), MCP-Actor Server (F), Token Economy (G), Session Isolation (I).
- **Deferred (post-MVP):** `start_debug` MCP verb, shared cross-session memory, HTTP/Run/DB tool surfaces (all Phase 2).

### A. Control-Surface Architecture *(the linchpin)*
A `ControlPort` trait defined in `domain`, with the concrete adapter selected by **Spike 0**. The system negotiates against three capability levels:
- **L0 — Observe:** read-only state (always achievable via JSONL/hooks).
- **L1 — Steer:** inject input / feedback into a session (the rejection-as-feedback primitive).
- **L2 — Govern:** force Plan mode, gate/intercept permissions.

If Spike 0 yields only L0, the product degrades to observe-and-notify (**NFR8**) with no rewrite — the engine and UI consume the same port, lighting up fewer features. **Affects:** FR12–20, FR30–32; the entire autonomy story.

> **✅ Spike 0 outcome (2026-06-02): GO at full-MVP.** L0 fully achievable; L2 substantial (PreToolUse `deny`+reason gates tools and takes precedence over allow rules; PermissionRequest auto-decides; plan mode set at spawn); L1 partial but sufficient for rejection-as-feedback (`permissionDecisionReason` + `additionalContext`). **One limitation:** cannot flip an *already-running* session to Plan externally — absorbed by the PDP (deny disallowed tools per phase) + SDK `resume` with `permissionMode: plan` for the next turn. Detection is better than assumed: transcript JSONL logs discrete `permission-mode` events + `ai-title`. Full findings: `.bmad-output/spike-0-findings.md`.
>
> **Integration posture (decided): Hybrid.** Primary `ControlPort` adapter = **Agent SDK** (spawn/drive sessions: streaming, per-tool callbacks, `permissionMode`, `resume`, interrupt) for full per-session governance. Secondary adapter = **global hooks + JSONL tail** (à la CodeIsland) to observe-and-gate sessions started outside MoonlightCode. The `control/` crate therefore ships both `sdk_adapter.rs` (primary) and `hook_adapter.rs` (secondary net), plus `observe_only.rs` (L0 fallback). The `detection/` crate consumes hooks + JSONL regardless of how a session was started.

### B. Detection Strategy
**Hybrid, event-first.** Claude Code hooks (`Notification`/`Stop`/`PreToolUse`) are the low-latency event source (**NFR1** ≤1s); session JSONL under `~/.claude/projects/` is the reconciling source of truth. A `detection` adapter fuses both, **infers** phase/blocked/done, parses defensively (artifacts are untrusted — **NFR6**), and biases toward false-positive nudges over missed blockers (**NFR5** ≥95%). **Affects:** FR3–5, FR12–13.

### C. Concurrency & Process Model
`tokio` async core. One supervised task per session owns its `portable-pty` child (spawn/attach/multiplex — FR9–10). An **event bus** (`tokio::broadcast` for fan-out events + `watch` for latest-state) decouples engine→GUI so the UI never blocks (**NFR4**) and updates propagate ≤1s. The fleet governor and HUD read-models live on this bus. **Affects:** FR1–11, FR37–41; NFR1–4.

### D. State, Persistence & Audit
**Embedded SQLite** (`sqlx`/`rusqlite`), two complementary shapes:
- **Append-only event log** — the audit trail and source of truth for one-click revert (FR33–34); also feeds the "since I last looked" review feed (FR22).
- **Derived state snapshots** — for fast restart/continuity (FR42) and the "what was I doing" summaries (FR43).
- Baseline metrics (FR45) as tables. Local-only, **zero telemetry** (**NFR12**); secrets to OS keychain, never here (**NFR9**). **Affects:** FR22, FR33–34, FR42–45.

### E. Trust & Permission Enforcement
A single **Policy Decision Point (PDP)** in `domain/trust` that every gated action funnels through — no scattered checks. It composes three inputs:
1. **Phase** (e.g. Plan = read-only) — FR15.
2. **Trust tier** (which MCP verbs auto-allowed) — FR30.
3. **Danger-zone list** (always-deny → explicit human approval, non-overridable by any tier) — FR32, **NFR10–11**.
A denial emits structured feedback into the session (FR19). **Affects:** FR15, FR19, FR30–32, FR51.

### F. MCP-Actor Server
Embedded **`rmcp` v0.16** (official Rust MCP SDK) server exposing actor verbs: `run_with_coverage`, `query_db`, `http_request`, `open_review` (MVP); `start_debug` (Phase 2). Every verb call passes through the PDP (E) before executing and writes to the audit log (D). **Affects:** FR25–29, FR31.

### G. Token Economy & Governor
RTK applied as a **command-wrapper layer** the supervisor wraps around session-run commands (RTK's proven model — FR37). The **fleet governor** reads rate-limit headroom off the event bus and pauses/down-shifts low-trust sessions when headroom drops (FR40); it must never throttle the Operator's own UI (**NFR4**). HUD is a read-model off the bus (FR38–39, FR41). RTK/OMC are optional — absence degrades, never breaks (**NFR15**). **Affects:** FR37–41.

### H. Conventions (errors / logging / config)
`thiserror` sentinel domain errors mapped at adapter boundaries; `tracing` injected structured spans feeding both diagnostics and the HUD read-model; YAML config aggregation (no env-var expansion by default) — the Rust mirror of the user's Go conventions (uber/config + Zap + `domain/errors`).

### I. Session Isolation Model — **Git worktree per session** (decided)
Each autonomous session gets its **own git worktree** off the attached repo (FR8, FR35): true parallel isolation, clean per-session diffs (feeds FR21–23 review), and straightforward revert. **File-lock negotiation (FR52)** is retained as a **safety net** for genuinely shared resources (shared config, a single dev DB, generated artifacts) that escape worktree boundaries — the MCP mediator arbitrates acquire/wait/release across sessions. Cost accepted: disk per worktree, per-worktree review/merge. **Affects:** FR8, FR35, FR52; NFR-safety.

### Decision Impact Analysis
**Implementation sequence (engine-up, gated by Spike 0):**
1. Spike 0 → choose ControlPort adapter + detection mechanism (A, B).
2. Engine + event bus + session supervisor/PTY (C).
3. Persistence + audit log (D).
4. PDP / trust enforcement (E) → unlocks safe autonomy.
5. MCP-actor server on top of PDP (F).
6. Token economy + governor + HUD read-models (G).
7. Worktree isolation + lock mediator (I).

**Cross-component dependencies:** ControlPort (A) capability level caps what E/F/the workflow machine can enforce. The event bus (C) is the spine everything else publishes/subscribes to. The PDP (E) sits between the MCP server (F) and execution. Persistence (D) backs audit (E/F), continuity (C), and review (the event log).

## Implementation Patterns & Consistency Rules

### Critical Conflict Points Identified
~9 areas where implementing agents could diverge: crate/module layout, port-vs-adapter placement, error handling, event naming, DB schema/migrations, async patterns, GPUI view structure, MCP verb contracts, and logging — each pinned below.

### Naming & Code Conventions
- **Crates:** kebab-case dirs, snake_case package names (`mcp-server` dir → `mcp_server`). **Modules/files:** snake_case (`session_supervisor.rs`, `phase_machine.rs`). Domain-first module dirs (`domain/src/session/`, not `services/`).
- **Types:** `PascalCase` structs/enums/traits; entities are plain nouns (`Session`, `ReviewHunk`, `TrustTier`, `AuditEntry`).
- **Traits = ports**, named by role with a noun/`-er` suffix, mirroring the Go interface style (`ControlPort`, `SessionStore`, `DetectionSource`, `Notifier`, `PolicyDecisionPoint`). Adapter structs implement them and are named concretely (`HookControlAdapter`, `SqliteSessionStore`, `JsonlDetectionSource`).
- **Functions/vars:** snake_case. **Constructors:** `new()` / `with_*()`; fallible construction returns `Result`.
- **Enums over booleans** for domain states (`Phase::{Plan,Auto,Test,Review,Commit}`, `SessionStatus::{Running,WaitingInput,Done,Errored}`) — never stringly-typed.

### Structure Patterns (ports & adapters)
- **`domain` crate depends on nothing** (no tokio, no I/O) — entities, trait ports, sentinel errors only. Adapters live in their own crates and implement domain traits. **UI/engine never call adapters directly** — always through a port (hexagonal rule).
- **Composition root = `apps/desktop`** only place that wires concrete adapters into ports (constructor injection of `Arc<dyn Trait>`); no global singletons, no service locator.
- **Tests:** unit tests co-located (`#[cfg(test)] mod tests` in-file); integration/BDD-style tests in each crate's `tests/`. Mirror the `tests/features` + fixtures habit where it adds value.

### Error Handling
- **Domain errors:** `thiserror` enums per domain module (`SessionError`, `ControlError`, `TrustError`) — the sentinel-error analogue of the Go `domain/errors`.
- **Adapter/app boundaries:** `anyhow::Result` acceptable in `apps/desktop` glue and adapter internals, but **public port methods return typed domain errors**, never `anyhow` leaking across a port.
- **No `unwrap()`/`expect()` in non-test code** except provably-infallible cases with a `// SAFETY:` comment. Errors surface to the UI as user-facing messages distinct from logs.

### Communication / Event Patterns
- **Event bus is the only engine→UI channel.** `EngineEvent::{SessionStateChanged, PhaseTransitioned, ReviewReady, ApprovalRequested, AuditAppended, GovernorAction, ...}` — `PascalCase` variants, past-tense for facts (`PhaseTransitioned`), imperative for requests (`ApprovalRequested`).
- **Payloads are owned structs** (no borrowing across the bus); every event carries `session_id` + monotonic `seq` + timestamp.
- **State updates are immutable** — engine owns canonical state; UI holds a read-model from `watch` channels. UI never mutates engine state directly; it sends `Command`s (`Command::{ToggleMode, RejectHunk, ApproveAction, SpawnSession, ...}`).

### Data / Persistence Patterns
- **SQLite tables:** snake_case plural (`sessions`, `audit_entries`, `review_hunks`, `baseline_metrics`). PKs `id` (ULID for time-ordering). FKs `<entity>_id`. Timestamps UTC as **epoch-millis** (chosen for ordering).
- **Migrations:** timestamped SQL files (`YYYYMMDDHHMMSS_description.sql`) under `crates/persistence/migrations/` — the prorizon convention. Forward-only.
- **Audit log is append-only**; revert is a *new compensating entry*, never a delete/mutate (FR34, NFR13).

### MCP Verb Contracts
- Each actor verb has a typed request/response struct in `domain` (e.g. `RunCoverageRequest`/`RunCoverageResult`); the `rmcp` adapter maps JSON↔these. **Every verb handler follows the same mandatory 5-step shape:** (1) resolve session → (2) PDP check → (3) execute → (4) audit-append → (5) return compact (RTK-shaped) result.

### Logging / Observability
- `tracing` everywhere; **one span per session-scoped operation**, named `op.<verb>` with a `session_id` field. Levels: `error` user-impacting, `warn` degraded/recovered, `info` lifecycle, `debug` detection internals. No `println!`. HUD telemetry derives from `tracing` + bus, not a parallel ad-hoc path.

### Enforcement Guidelines
**All implementing agents MUST:** keep `domain` I/O-free; access data only through ports; return typed domain errors across ports; publish state changes only via `EngineEvent`; route every gated/autonomous action through the PDP then the audit log; add a migration file for any schema change; no `unwrap` in prod code; no `println!`.
**Verification:** `cargo clippy` (deny warnings) + a CI check that `domain` has no tokio/I/O dependency; PR self-review against this list.

**Anti-patterns to reject:** UI mutating engine state; an adapter imported into `domain`; a permission check outside the PDP; stringly-typed phase/status; silent `Result` swallowing; bypassing the audit log for an autonomous action.

## Project Structure & Boundaries

### Complete Project Directory Structure

```
moonlightcode/                          # single repo, single `cargo build`
├── Cargo.toml                          # workspace manifest (members + shared deps via [workspace.dependencies])
├── Cargo.lock
├── rust-toolchain.toml                 # pin stable channel
├── clippy.toml                         # deny-warnings config
├── rustfmt.toml
├── README.md
├── AGENTS.md                           # house rules for implementing agents (mirrors Go AGENTS.md)
├── .github/workflows/ci.yml            # fmt + clippy(-Dwarnings) + test + domain-purity check
├── docs/
│   └── spike-0/                        # throwaway prototype findings (control surface + detection)
│
├── crates/
│   ├── domain/                         # PURE: entities + ports (traits) + sentinel errors; NO tokio/IO
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── session/                # Session, SessionStatus, SessionId        → FR1-10
│   │       ├── phase/                  # Phase enum, transitions, PhaseMachine rules → FR12-17
│   │       ├── review/                 # ReviewHunk, Decision, Feedback            → FR18-24
│   │       ├── trust/                  # TrustTier, DangerZone, PolicyDecisionPoint (trait) → FR15,30-32,51
│   │       ├── audit/                  # AuditEntry, RevertEntry                   → FR33-34
│   │       ├── economy/                # TokenStat, GovernorPolicy, RateHeadroom   → FR37-41
│   │       ├── continuity/             # SessionSummary, BaselineMetric            → FR42-45
│   │       ├── ports/                  # ALL trait ports gathered:
│   │       │   ├── control.rs          #   ControlPort (L0/L1/L2)                  → FR12-20 (write side)
│   │       │   ├── detection.rs        #   DetectionSource                        → FR3-5
│   │       │   ├── store.rs            #   SessionStore, AuditStore, BaselineStore → FR33-34,42-45
│   │       │   ├── mcp.rs              #   ActorVerb contracts (req/res structs)   → FR25-29
│   │       │   ├── notifier.rs         #   Notifier (OS notifications)            → FR5
│   │       │   └── locks.rs            #   FileLockMediator                       → FR52
│   │       └── errors.rs               # SessionError, ControlError, TrustError, ...
│   │
│   ├── engine/                         # core orchestrator (tokio); depends on domain (ports only)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── supervisor.rs           # spawn/track/attach session tasks + PTY    → FR9-10
│   │       ├── phase_machine.rs        # runs Phase transitions, auto-revert       → FR12-14
│   │       ├── bus.rs                  # EngineEvent broadcast + watch read-models → NFR1-4
│   │       ├── governor.rs             # fleet token governor                      → FR40
│   │       ├── reviewqueue.rs          # cross-session "since I last looked"        → FR22
│   │       └── commands.rs             # Command handlers (UI→engine)              → FR14,18,20
│   │
│   ├── detection/                      # adapter: impl DetectionSource
│   │   └── src/
│   │       ├── hooks.rs                # CC hook listener (Notification/Stop/PreToolUse)
│   │       ├── jsonl.rs                # ~/.claude/projects/*.jsonl tailer (defensive parse)
│   │       └── fuse.rs                 # fuse events→inferred phase/blocked/done   → FR3, NFR5-6
│   │
│   ├── control/                        # adapter: impl ControlPort (Spike-0-chosen mechanism)
│   │   └── src/
│   │       ├── hook_adapter.rs         # HookControlAdapter (L1/L2 attempt)
│   │       ├── sdk_adapter.rs          # AgentSdkControlAdapter (alt)
│   │       └── observe_only.rs         # L0 fallback (NFR8 graceful degradation)
│   │
│   ├── mcp-server/                     # adapter: embedded rmcp server exposing actor verbs
│   │   └── src/
│   │       ├── server.rs               # rmcp wiring, transport
│   │       └── verbs/                  # run_with_coverage, query_db, http_request, open_review → FR25-28
│   │                                   #   (each verb: resolve→PDP→exec→audit→compact-result)
│   │
│   ├── trust/                          # adapter: PolicyDecisionPoint impl
│   │   └── src/pdp.rs                  # composes phase + tier + danger-zone        → FR15,30-32
│   │
│   ├── persistence/                    # adapter: SQLite stores
│   │   ├── migrations/                 # YYYYMMDDHHMMSS_*.sql (forward-only)
│   │   └── src/                        # SqliteSessionStore, SqliteAuditStore (append-only), Baseline
│   │
│   ├── worktree/                       # adapter: git worktree isolation + FileLockMediator → FR35,52
│   │   └── src/{worktree.rs, locks.rs}
│   │
│   ├── rtk/                            # adapter: RTK command-wrapper integration (optional) → FR37
│   │   └── src/wrapper.rs
│   │
│   └── core/                           # cross-cutting: config(YAML), tracing setup, keychain, util
│       └── src/{config.rs, logging.rs, secrets.rs}   → NFR9,12; HUD telemetry source
│
└── apps/
    └── desktop/                        # GPUI app = COMPOSITION ROOT (the only wiring place)
        ├── Cargo.toml
        ├── assets/                     # icons, themes
        └── src/
            ├── main.rs                 # build adapters → inject into engine ports → run GPUI
            ├── wiring.rs               # Arc<dyn Trait> composition (no globals)
            ├── views/
            │   ├── grid.rs             # session tiles, traffic-light, focus     → FR1-3,7
            │   ├── needs_you.rs        # prioritized blocked-queue                → FR4,6
            │   ├── review.rs           # diff + per-hunk accept/reject            → FR21,23
            │   ├── terminal.rs         # portable-pty terminal panes (multiplex)  → FR10
            │   ├── approvals.rs        # danger-zone / trust prompts, GO/NO-GO     → FR31-32,51
            │   ├── hud.rs              # context%, token burn, governor, saved     → FR38-39,41
            │   └── workspace.rs        # dockable panel layout, calm-by-default    → NFR18-19
            └── read_model.rs           # subscribes to engine watch channels (UI state)
```

### Architectural Boundaries
- **The one law:** dependencies point inward to `domain`. `domain` imports nothing app-specific (CI enforces no tokio/IO in `domain`). `engine` depends only on `domain` *ports*. Adapter crates implement ports. `apps/desktop` is the only crate that knows concrete adapter types.
- **Control-surface boundary:** everything that touches Claude Code goes through `ControlPort` (write/govern) or `DetectionSource` (read) — nothing else imports `control`/`detection` internals. This is what makes the Spike-0 outcome swappable and the L0 fallback free.
- **Permission boundary:** the **only** path to a gated/autonomous action is through the `PolicyDecisionPoint`; the `mcp-server` verbs and `engine` commands both call it. No check lives anywhere else.
- **UI↔engine boundary:** UI reads via `watch` read-models, writes via `Command`s. UI never holds an adapter or mutates engine state. Guarantees ≤1s propagation (NFR1) and non-blocking UI (NFR4).
- **Data boundary:** all persistence behind `*Store` ports; the audit store is append-only; secrets never touch SQLite (keychain via `core::secrets`).

### Requirements → Structure Mapping
| Capability area | Primary location |
|---|---|
| Orchestration/awareness (FR1–11) | `engine/supervisor`, `apps/desktop/views/{grid,needs_you,terminal}`, `detection` |
| Workflow/phase (FR12–17) | `domain/phase`, `engine/phase_machine`, `control` |
| Steering/feedback (FR18–20) | `domain/review`, `engine/commands`, `control` (L1) |
| Review (FR21–24) | `engine/reviewqueue`, `apps/desktop/views/review`, audit event log |
| MCP-actor (FR25–31) | `mcp-server/verbs`, `trust/pdp` |
| Safety/audit (FR32–36,51) | `trust`, `persistence` (append-only), `apps/desktop/views/approvals` |
| Token economy (FR37–41) | `engine/governor`, `rtk`, `core`, `apps/desktop/views/hud` |
| Continuity (FR42–45) | `persistence`, `domain/continuity` |
| Setup/baseline (FR45–46) | `core/config`, `persistence`, first-run detection |
| Isolation/locks (FR35,52) | `worktree` |

### Integration Points
- **Internal:** all async over the `engine` event bus (`EngineEvent` out, `Command` in). No direct cross-adapter calls.
- **External:** Claude Code (via `control`+`detection`), RTK (via `rtk` wrapper, optional), OS keychain (`core::secrets`), OS notifications (`Notifier` adapter), git (`worktree`). All optional-external degrade gracefully (NFR8, NFR15).
- **Data flow (typical autonomous verb):** Agent → `mcp-server` verb → `PolicyDecisionPoint` → execute (possibly RTK-wrapped) → append `AuditStore` → emit `EngineEvent::AuditAppended` → UI read-model + HUD update.

### Development Workflow
- **Build:** one `cargo build` at the workspace root; `apps/desktop` is the binary. **Run:** `cargo run -p desktop`.
- **CI:** `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, plus a domain-purity check (no tokio/IO deps in `domain`).
- **Spike 0** lives in `docs/spike-0/` as throwaway code/notes and does **not** block the workspace compiling.

## Architecture Validation Results

### Coherence Validation ✅
- **Decision compatibility:** All choices compose cleanly — GPUI (single-build Rust) + `tokio` engine + `rmcp` server + SQLite + `portable-pty` are mutually compatible and version-verified (June 2026). No contradictions.
- **Pattern consistency:** The ports-and-adapters layering, event-bus-only UI channel, and single-PDP rule are consistently expressible in the chosen stack and mirror the user's Go conventions.
- **Structure alignment:** The Cargo workspace enforces the boundaries (domain-purity via CI, composition root in `apps/desktop`); structure supports every decision.

### Requirements Coverage Validation
- **Functional (52 FRs):** All MVP FRs have an explicit home in the structure map. Phase-2 FRs (11, 24, 29, 44, 47–50) are intentionally unbuilt but have reserved locations. Three minor gaps found and resolved (below).
- **Non-Functional (20 NFRs):** All addressed architecturally — perf (event bus + async, NFR1–4), reliability (detection fuse + defensive parse + restart + observe-only fallback, NFR5–8), security/privacy (PDP + danger-zone + keychain + local-only append-only audit, NFR9–13), integration (control port + optional adapters + macOS-first, NFR14–17), usability (calm workspace + keyboard, NFR18–20).

### Gap Analysis & Resolutions
**Minor gaps (resolved inline — no blockers):**
1. **FR16 "encode your loop" workflow templates** → template *definitions* in `core/config` (YAML), template *semantics* (phase sequence + default trust tier) in `domain/phase`; applied by `engine/supervisor` on spawn.
2. **NFR19 keyboard-first / command palette** → add `apps/desktop/src/views/command_palette.rs` + an input/keymap layer; all `Command`s must be palette-addressable.
3. **DND / focus notification batching** (Journey 1 + NFR18) → an explicit responsibility of the `Notifier` adapter (batching policy lives there, not scattered in views).

**No critical or important gaps.** Phase-2 scope is deliberately deferred, not missing.

### Architecture Completeness Checklist
- ✅ Requirements analysis — context, scale, constraints, cross-cutting concerns
- ✅ Architectural decisions — A–I documented, versions verified
- ✅ Implementation patterns — naming, structure, errors, events, data, MCP, logging
- ✅ Project structure — full Cargo workspace tree, boundaries, FR mapping
- ✅ Gap analysis — 3 minors resolved, 0 blockers

### Architecture Readiness Assessment
**Overall status:** READY FOR IMPLEMENTATION — **Spike 0 passed; Risk #1 retired.**
**Confidence level: High.** Spike 0 (2026-06-02) confirmed L0 fully, L2 substantially, L1 sufficiently — the full-MVP ambition is viable, not degraded. The riskiest unknown is now resolved with corroborating evidence (local install + authoritative docs + CodeIsland precedent).

**Resolved linchpin:** The workflow-machine thesis (force-Plan-ish via PDP gating, permission gating, autonomous MCP-actor — FR12–17, FR30–32) is achievable via `PreToolUse → deny` + SDK `permissionMode`/`resume`. The single remaining limitation (no live external mode-flip on a running session) is absorbed by the PDP design and does not reduce scope. Integration posture decided: **Hybrid** (SDK-primary + hooks/JSONL secondary).

**Key strengths:** single-build pure-Rust; risk contained behind ports; one permission authority; event-driven ≤1s; local-first/zero-telemetry; mirrors the user's proven Go conventions.

**Future enhancement areas:** GPUI→egui fallback if pre-1.0 instability bites; Phase-2 tool surfaces; Linux port; analyst stats.

### Implementation Handoff
- **AI agents must:** follow A–I decisions exactly; obey the patterns/anti-patterns list; respect the ports boundary and single-PDP rule; consult this doc for any architectural question.
- **First implementation priority:** ~~Spike 0~~ **✅ done (2026-06-02, GO).** Next: scaffold the Cargo workspace, then engine-up per the decision sequence (supervisor + bus → persistence → PDP → MCP server → economy → worktree), with the **Hybrid `ControlPort`** (SDK-primary + hooks/JSONL secondary) as the integration spine.
