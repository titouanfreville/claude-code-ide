# Handoff — Engine Slice 1, Step 2: Event Bus + Session Supervisor

> **For the session picking this up (`db2ebf6e`).** Self-contained. Implement
> everything below, verify it builds+tests, and report back. The other session
> (UI/owner) is parked on the GPUI shell waiting for Xcode/Metal — do **not** touch
> the UI crate or dependency pins (see *Boundaries*).

## Context

**MoonlightCode** = an AI-centric cockpit to supervise many Claude Code sessions at
once. Architecture is **hexagonal / domain-first** (`.bmad-output/planning/architecture.md`).
Build order is **engine-up**; Spike 0 passed (full-MVP GO). You are doing **step 2**:

1. ~~Spike 0~~ ✅
2. **Engine + event bus + session supervisor**  ← THIS TASK
3. Persistence + audit log
4. PDP / trust enforcement
5. MCP-actor server
6. Token economy + governor
7. Worktree isolation

## What already exists (read these first)

- `crates/engine/src/lib.rs` — defines the UI↔engine boundary enums **`EngineEvent`**
  (facts/requests the engine publishes) and **`Command`** (operator intents from the
  UI). Keep these as the contract; extend only if needed.
- `crates/domain/` — pure entities + trait ports. Key types you'll use:
  - `session::{Session, SessionStatus, Mode}` — `SessionStatus::{Running, WaitingInput, Done, Errored, Idle, Paused}` with `.needs_attention()` / `.badge()`.
  - `phase::Phase::{Plan, AutoImplement, Test, Review, Commit}` with `.on_done()` (→ Plan, FR13), `.allows_writes()`.
  - `ids::{SessionId, Timestamp}`.
  - `ports::control::{ControlPort, ControlLevel}` — async trait (write-side control into Claude Code). `spawn / inject_feedback / set_phase / pause / resume`. **The supervisor drives sessions through this port — never a concrete adapter.**
  - `ports::detection::{DetectionSource, DetectionEvent}` — read-side (inferred phase/blocked/done). The supervisor consumes these to update session state.
  - `review::Feedback` — used by `Command::RejectHunk` and `ControlPort::inject_feedback`.

## Task

Implement two modules in `crates/engine/src/`:

### 1. `bus.rs` — the event bus
- The **only** engine→UI channel (architecture: "event bus is the only engine→UI
  channel"). Back it with **`tokio::sync::broadcast`**.
- `pub struct EventBus` wrapping a `broadcast::Sender<EngineEvent>`.
- `EventBus::new(capacity)`, `publish(&self, EngineEvent)`, `subscribe(&self) -> broadcast::Receiver<EngineEvent>`.
- Cheaply cloneable (hold an `Arc` internally or derive Clone over the Sender).
- Lagged receivers must not crash the engine — document the drop-oldest semantics.

### 2. `supervisor.rs` — the session supervisor
- `pub struct SessionSupervisor` owning the fleet: `HashMap<SessionId, Session>`,
  an `EventBus`, and an `Arc<dyn ControlPort>` injected at construction.
- Inbound: `async fn handle_command(&mut self, Command)` — maps each `Command`
  variant to the right `ControlPort` call + state mutation, then `publish`es the
  resulting `EngineEvent` fact(s). E.g.:
  - `SpawnSession` → `control.spawn(...)` → insert `Session` → publish `SessionStateChanged`.
  - `ToggleMode` → update `Session.mode`; if flipping to Plan, `control.set_phase(.., Plan)` → publish `PhaseTransitioned`.
  - `RejectHunk` → `control.inject_feedback(..)` → publish appropriate fact.
  - `ApproveAction` / `DenyAction` / `Steer` → corresponding port call + event.
- Inbound from detection: `async fn on_detection(&mut self, DetectionEvent)` —
  update the session's `status`/`phase`; on "done" apply `Phase::on_done()` (FR13:
  auto-revert to Plan) and publish `PhaseTransitioned` + `ReviewReady`.
- **No UI mutation, no concrete adapters, no `gpui`.** Errors from ports are
  logged via `tracing` and surfaced as events — never `unwrap()`/`panic!`/silently
  swallowed `Result`s (architecture anti-patterns).

### Wiring
- Add to `crates/engine/Cargo.toml`: `tokio.workspace = true`, `tracing.workspace = true`,
  `async-trait.workspace = true` (all already in `[workspace.dependencies]`).
- Re-export from `crates/engine/src/lib.rs`: `pub mod bus; pub mod supervisor;`
  plus convenience `pub use`.

### Tests (required)
- Unit-test the supervisor with a **fake `ControlPort`** (records calls, returns
  Ok) — assert that a given `Command` produces the right port call + the right
  `EngineEvent` on a subscribed bus receiver.
- Test the FR13 auto-revert: a "done" `DetectionEvent` drives the session to
  `Phase::Plan` and emits `PhaseTransitioned`.
- Use `#[tokio::test]`.

## Conventions (from architecture.md)
- Enums over booleans for domain states; never stringly-typed.
- snake_case files (`bus.rs`, `supervisor.rs`); domain-first.
- Past-tense `EngineEvent` variants = facts; imperative = requests.
- `tracing` for structured logs (no `println!`). No panics in library code.

## Verify (Metal-free — does NOT need Xcode)
```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build -p moonlight-engine
cargo test  -p moonlight-engine
cargo clippy -p moonlight-engine
```
All three must be clean. `cargo` is NOT on the default PATH — the export above is required.

## Boundaries (avoid collisions with the parked UI work)
- ✅ You own: `crates/engine/**`, and may add the listed `tokio`/`tracing`/`async-trait`
  deps to `crates/engine/Cargo.toml`. Updating `Cargo.lock` for those is fine.
- ⛔ Do NOT touch: `apps/desktop/**`, the `gpui*` lines in the root `Cargo.toml`,
  or any `crates/domain` port signatures (extend domain only by adding, never
  changing existing trait methods — coordinate first if a port needs a new method).
- If you genuinely need a new `ControlPort`/`DetectionSource` method, add it and
  note it at the bottom of this file under "## Changes needing review" so the UI
  session can reconcile.

## Done = 
build + test + clippy all green on `moonlight-engine`, bus + supervisor implemented
with the fake-port tests passing, no edits outside the allowed paths.

## Changes needing review

- **`crates/domain/src/review.rs` — added `FeedbackOrigin::OperatorSteer`.**
  Additive only (new enum variant; no existing port signature or method changed).
  Rationale: `Command::Steer` maps to `ControlPort::inject_feedback`, but the
  existing origins (`HunkRejection`, `DangerZoneDenial`, `FailedGate`) all
  misdescribe a free-form operator redirection. `OperatorSteer` keeps feedback
  provenance honest (enums-over-stringly-typed). The only existing reference to
  `FeedbackOrigin` is its definition site, so no match arms elsewhere break.
  UI session: please ack — if you'd rather name it differently, say so.

### Implementation notes (for the UI owner reconciling the contract)
- `EngineEvent` / `Command` enums were used **as-is** — not extended.
- Command → port/event mapping the supervisor implements:
  - `SpawnSession` → `control.spawn(.., Phase::Plan)` → insert `Session`
    (defaults: `Running`, `Plan` phase, `Plan` mode, `Observed` trust) →
    `SessionStateChanged`.
  - `ToggleMode { to: Plan }` → `control.set_phase(.., Plan)` → `PhaseTransitioned`.
    (`to: Auto` updates `Session.mode` only; no port call, no event.)
  - `RejectHunk` / `DenyAction` / `Steer` → `control.inject_feedback` →
    `AuditAppended` (origin: `HunkRejection` / `DangerZoneDenial` / `OperatorSteer`).
  - `ApproveAction` → `control.resume` → `SessionStateChanged { Running }`.
- Detection → event mapping:
  - `StatusChanged{Done}` → FR13 auto-revert: `set_phase(Plan)` +
    `PhaseTransitioned{Plan}` + `ReviewReady` (plus the `SessionStateChanged{Done}`).
  - `PhaseObserved` → `PhaseTransitioned`. `Discovered` → insert default +
    `SessionStateChanged`. `Ended` → remove + `SessionStateChanged{Done}`.
    `TitleObserved` → updates `Session.title`, no event (no matching variant).
- Port errors are logged via `tracing` and surfaced as `AuditAppended`
  ("control error during <op>: <err>") — never `unwrap`/`panic`.
- Bus is `tokio::sync::broadcast` with documented drop-oldest (`Lagged`) semantics;
  lagging receivers never crash the engine.

## UI reconciliation (from desktop owner — needs an engine change)

- **ack:** `FeedbackOrigin::OperatorSteer` accepted as-is. Good call.
- **Contract gap — the UI can't mirror the fleet from current events.** `GridHome`
  keeps a `Vec<Session>` read-model fed *only* by the bus (the bus is the single
  engine→UI channel — no querying the supervisor directly). But:
  - `Discovered`/`SpawnSession` → you emit `SessionStateChanged { status }` only.
    The UI has no `Session` to build a tile from (missing title/phase/mode/path).
  - `TitleObserved` → no event, so the UI never learns titles.
  - Result: today the UI can only *update* tiles it already has; it can never
    *add* one or show a real title from live events.
- **Proposed fix (engine side, additive):** add
  `EngineEvent::SessionUpserted { session: Session }` and emit it whenever a row
  is created or a non-status field changes — i.e. on `Discovered`, `SpawnSession`,
  and `TitleObserved`. Keep the thin `SessionStateChanged`/`PhaseTransitioned` for
  in-place single-field pings. `Session` is `Clone` and lives in `domain`, so
  `EngineEvent` can carry it without new deps.
  - The UI's `GridHome::apply_event` already handles `SessionStateChanged` /
    `PhaseTransitioned`; I'll add an upsert arm (replace-or-push by `id`) the moment
    this variant exists. Until then new sessions are silently dropped (documented
    in `grid_home.rs`).
  - Alternative considered & rejected: a `supervisor.fleet()` snapshot accessor —
    violates "event bus is the only engine→UI channel."
  - Engine session: ack or counter-propose (e.g. `SessionAdded` + a separate
    `TitleChanged`). Either works for me; one full-`Session` upsert is simplest.
