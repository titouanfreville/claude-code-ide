---
title: "Control Adapter Design — Hook-based Gating (Path A)"
status: "draft"
date: "2026-06-02"
relates_to:
  - ".bmad-output/planning/architecture.md (Decision A — ControlPort)"
  - ".bmad-output/spike-0-findings.md (Path A vs B)"
---

# Control Adapter — Hook-based Gating

## 1. Decision & principle

**MoonlightCode enriches Claude Code; it never owns or bypasses sessions.** Sessions
are launched and owned by Claude Code (TUI / IDE / CLI). We attach, observe (JSONL —
done), and **govern via the hooks CC already runs** — Spike 0 "Path A".

Hard rule: **if MoonlightCode is not running, the user's Claude Code must behave
exactly as normal.** Every hook fails *open* (allow) on any error/timeout. We are
additive, never a hard dependency.

## 2. The control loop

```
Claude Code session (user-owned)
   │  PreToolUse / PermissionRequest / UserPromptSubmit / Stop hooks (configured once)
   ▼
moonlight-hook  (CLI; reads hook JSON on stdin, writes hook JSON on stdout)
   │  connect ~/.moonlight/control.sock, send request, read reply  (timeout → fail-open)
   ▼
MoonlightCode app — Control Server
   │  reads the per-session Gate read-model + classifies the tool → PDP.decide()
   ▼
{ allow | deny, reason }  → moonlight-hook prints CC-shaped response → CC enforces
```

We only answer questions CC already asks. No agent-loop control, no spawning.

## 3. ControlPort semantics under this posture

The existing `domain::ports::ControlPort` trait stays; the `HooksControl` adapter
implements it with hook semantics:

| Method | Behaviour |
|---|---|
| `spawn` | `Err(Unavailable)` — CC owns launching. |
| `set_phase(s, p)` | Records desired phase for `s` (operator intent). Enforced by the PDP denying disallowed tools on the next call. No external mode-flip (Spike 0 caveat). |
| `inject_feedback(f)` | Queues feedback for `s`; delivered as `permissionDecisionReason` on a deny, or `additionalContext` on the next `UserPromptSubmit`. |
| `pause` / `resume` | `pause` = PDP denies all tools (read-only halt); `resume` clears it. |
| `level()` | `Govern` (we can deny) — degrades to `Observe` if hooks aren't installed. |

## 4. Architecture — control is just another bus subscriber

The Control Server does **not** couple to the supervisor. Like the UI, it subscribes
to the `EventBus` and folds events into a fast in-memory **Gate read-model**:

```
engine (supervisor) ──EngineEvent──► EventBus ──► UI (GridHome)
                                            └────► Control Server  → Gate read-model
                                                                     Arc<RwLock<HashMap<SessionId, GateState>>>
```

`GateState { phase: Phase, trust: TrustTier, paused: bool }`, updated from
`SessionUpserted` / `PhaseTransitioned` / `SessionStateChanged`. Operator intent
(set_phase/pause via `Command`) flows supervisor→bus→read-model, so the gate always
reflects what the operator sees. Bus stays the single engine→consumer channel.

Hook reads are O(1) on the read-model behind an `RwLock` (read-mostly) — fast enough
for synchronous hook calls.

## 5. IPC protocol

- **Transport:** Unix domain socket `~/.moonlight/control.sock`, perms `0600`.
- **Framing:** one JSON request line in, one JSON response line out, then close.
- **Request** (subset of CC's documented hook payload — untrusted, parsed defensively):
  ```json
  { "event": "PreToolUse", "session_id": "…", "tool_name": "Edit",
    "tool_input": { … }, "cwd": "…" }
  ```
- **Response:**
  ```json
  { "decision": "allow" }
  { "decision": "deny", "reason": "MoonlightCode: writes disabled in Plan phase" }
  ```
- `moonlight-hook` maps the response to CC's hook output shape
  (`permissionDecision` / `permissionDecisionReason`, or exit code 2 + stderr).

## 6. Tool classification

CC tool name → action class (conservative; artifacts untrusted):

| Tools | Class |
|---|---|
| `Read`, `Grep`, `Glob`, `LS` | read |
| `Edit`, `Write`, `MultiEdit`, `NotebookEdit` | write |
| `Bash` | write + danger (can mutate anything) |
| MCP verbs | per `McpVerb` danger mapping |

The PDP composes class + session phase + trust tier (+ danger zone) → allow/deny.
Phase gate uses the existing `Phase::allows_writes()` (Plan ⇒ false).

## 7. Hook registration

Hooks live in `~/.claude/settings.json` (global) — registering them is an
**outward-facing change to the user's CC config**. Policy: **never auto-edit; emit a
snippet for the user to paste**, unless explicitly told to write it. Example:
```json
{ "hooks": { "PreToolUse": [ { "matcher": "*",
  "hooks": [ { "type": "command", "command": "moonlight hook pre-tool-use" } ] } ] } }
```
A later `moonlight hooks install` command can manage this with a backup + confirm.

## 8. File layout (additive)

```
crates/control/src/
  lib.rs        # ObserveOnlyControl (exists) + HooksControl (ControlPort)
  ipc.rs        # HookRequest/HookResponse types + socket framing
  server.rs     # listener loop; subscribes bus → Gate read-model; routes → PDP
  gate.rs       # GateState + read-model fold
  classify.rs   # CC tool name → action class
apps/desktop/src/main.rs   # `moonlight hook <event>` subcommand (client) + start server task
```

## 9. Failure modes (all fail-open)

- Socket missing / app down → allow (CC normal).
- Timeout (> ~300ms) → allow, log on app side next start.
- Unparsable payload → allow.
- Unknown session_id → allow (we only gate sessions we track).

Fail-*closed* is opt-in later (a "strict" mode) — default must be safe-for-CC.

## 10. First slice — "Plan-phase write gate"

Scope:
1. `ipc.rs` types + Unix-socket server (`server.rs`) subscribing to the bus → Gate read-model (`gate.rs`).
2. `classify.rs` (read vs write; Bash = write).
3. PDP decision = `deny` when tool is write and the session's phase disallows writes; else allow. (Reuse `DefaultPdp`; extend minimally.)
4. `moonlight hook pre-tool-use` subcommand (stdin→socket→stdout, fail-open).
5. Wire the server task into `main` next to detection. Emit the settings snippet (no auto-edit).

**Acceptance:** with a real CC session attached to a repo and shown in **Plan** in
MoonlightCode, an `Edit`/`Write` attempt is denied with
`MoonlightCode: writes disabled in Plan phase`; flipping the session to Auto allows it;
with MoonlightCode closed, the same edit proceeds normally (fail-open).

Tests: classify (table), gate fold (events→GateState), PDP decision (phase×class),
hook client fail-open (no socket → allow). The end-to-end CC test is manual.

## 10b. Confirmed decisions (2026-06-02)

- **Gate scope = adopted sessions only.** Deny applies only to sessions the operator
  explicitly *adopts*. Discovered-but-unadopted sessions are observe-only. Adoption is
  operator state in the Gate read-model (`adopted: bool`), toggled via a `Command`.
- **Bash is parsed now.** `classify` inspects the Bash command string (e.g.
  `ls/cat/grep/git status` → read; `rm/mv/git commit/>` redirection → write). Conservative
  default = write when unsure.
- **Build the installer.** Ship `moonlight hooks install` (backup + confirm + write
  `~/.claude/settings.json`) in addition to printing the snippet.

### Component placement (keeps `control` domain-pure)
`crates/control` depends on **domain only**. The bus→GateState fold needs `EngineEvent`
(in `engine`), so it lives in the **app** (composition root), which passes the resulting
`Arc<RwLock<HashMap<SessionId, GateState>>>` (plain domain data) into the control server.
This also keeps control work clear of the parallel terminal/dock UI work in `apps/desktop`.

### Revised increment order (conflict-isolated)
- **Increment 1 (now): decision core — `crates/control` + `crates/trust` only, no app/socket.**
  `classify.rs` (with Bash parsing), `ipc.rs` types, `gate.rs` (`GateState` + `decide`),
  and flesh out `DefaultPdp` for the phase×class gate. Fully unit-tested. Zero `apps/desktop` edits.
- **Increment 2: transport.** Unix-socket `ControlServer` + `moonlight hook` subcommand (fail-open).
- **Increment 3: wiring + adoption UI.** Bus→GateState fold in app, `Command::ToggleAdoption`, adopt affordance.
- **Increment 4: installer** `moonlight hooks install`.

## 11. Open questions

- **Phase source for user-launched sessions.** A discovered (not operator-set) session
  has phase from JSONL `permission-mode`. Is observed-plan enough to gate on, or only
  gate when the operator explicitly set Plan? (Proposed: gate on effective phase, but
  only deny for sessions the operator has "adopted" — avoids surprising the user on day one.)
- **Bash granularity.** Treat all Bash as write for slice 1; refine with command parsing later.
- **Latency budget.** Confirm acceptable hook overhead; 300ms timeout proposed.
- **Multiple app instances / stale socket.** Single-instance lock on the socket.
```
