# Plan-View Map — investigation report

Scope: how MoonlightCode renders the "plan view" (the clean formatted plan shown to the
operator when an agent proposes a plan via Claude's `ExitPlanMode`, or an agent-produced
plan). Covers the render panel, the data/trigger wiring, the Gemini/AGY path, and the
handoff-doc history. All refs are `file:line` at the time of writing.

---

## KEY FINDINGS

**(a) Broken vs never-populated? → POPULATED but MIS-STYLED (broken-looking), not empty.**
For Claude sessions the plan *is* captured and *is* rendered as real markdown. The panel
calls `TextView::markdown("plan-body-md", self.plan.clone())` at
`apps/desktop/src/views/panels/plan_review.rs:568`. The problem is it is rendered with the
**default `TextViewStyle`** (`is_dark: false`, light `HighlightTheme`) on top of a **dark**
`surface_raised()` background — so headings/code-blocks/inline colors are light-theme values
washed out on a dark card. There is **no `.style(...)`** on that call, and no
`theme::markdown_style()` helper exists (grep of `views/theme.rs` for `markdown`/`TextView`
returns nothing). This is exactly the unfinished T14 fix described in
`.ai/handoffs/01-t14-and-fullscreen-plan.md` — that fix was planned but **not applied**
(the helper is absent and the call site is unstyled). So: not "never populated", and not
"raw text / no markdown parse" — it *is* parsed markdown, just themed wrong.

**(b) Trigger gap for auto-mode and Gemini.**
- The plan tab is triggered by the **`ExitPlanMode` tool call**, not by the workflow "Plan"
  phase. In `crates/control/src/gate.rs:110-114`, `evaluate()` holds *only* when
  `tool_name == EXIT_PLAN_MODE` ("ExitPlanMode"), independent of phase. So it fires whenever
  Claude emits `ExitPlanMode` for an adopted, non-paused session — regardless of Discovery /
  Plan / Auto. In practice Claude only emits `ExitPlanMode` while in its native *plan*
  permission-mode, so in **auto mode no plan tab appears** — but that is a Claude-behavior
  gap (Claude doesn't call the tool), not a MoonlightCode phase gate. There is no
  auto-mode-produced "here is my plan" surface.
- **Gemini/AGY produces NO plan view at all.** The AGY hook normalizer
  (`apps/desktop/src/agy_hook.rs:63-98 map_tool`) has **no `ExitPlanMode` / plan-tool
  mapping**. AGY's native Plan Mode (`/plan`, `/planning`, per handoff 04) is not surfaced as
  an `ExitPlanMode` `PreToolUse`, so the gate never returns `HoldKind::Plan` → `PlanProposed`
  is never emitted for AGY. The second emission path (JSONL tail, `crates/detection`) is
  Claude-specific (parses `~/.claude` JSONL `ExitPlanMode` tool_use blocks) and does not read
  Gemini transcripts. Net: AGY plans reach neither emission path, so they never open the
  `PlanReviewPanel`.

**(c) Which struct / render fn needs restyling.**
`PlanReviewPanel` (struct at `plan_review.rs:42`), specifically `impl Render for
PlanReviewPanel::render` (`plan_review.rs:511-577`) — the `TextView::markdown(...)` child at
**`plan_review.rs:568`**. Fix = add a dark `theme::markdown_style()` (does not exist yet) and
apply `.style(theme::markdown_style())` to that `TextView`. (Same helper would also fix the
assistant-message markdown in `session_monitor.rs`, per handoff 01.) The surrounding chrome
(header, bordered scroll body, footer action bar) is already styled with the theme tokens.

---

## 1. Files/modules under `apps/desktop/src` that render (or wire) the plan view

| File | Role |
|---|---|
| `apps/desktop/src/views/panels/plan_review.rs` | **THE plan view.** `PlanReviewPanel` struct + `Render` impl + approve/refine/reject action bar. (577 lines) |
| `apps/desktop/src/views/panels/mod.rs:13,33` | Registers the `plan_review` module: `pub mod plan_review;` — "shows a plan the agent proposed (center tab; plan-review gate)". |
| `apps/desktop/src/views/workspace.rs:66,321-327,685-700,2118-2121,2296,2377` | Panel registration (`register_panel("PlanReview", …)`), the bus→center subscriber that opens the tab on `PlanProposed`, and layout rehydration of a restored `PlanReview` tab. |
| `apps/desktop/src/views/center_requests.rs:31-35,85,107` | `OpenRequest::PlanReview { session, plan, root }` variant + dedup key `plan:<session>` + `target_root()`. |
| `apps/desktop/src/main.rs:626-657` | `BusNotifier::approval_requested` — the **hook→bus** emitter of `EngineEvent::PlanProposed`. |
| `apps/desktop/src/views/grid_home.rs:138` | Explicitly *ignores* `PlanProposed` for the tile read-model (handled by the plan view instead). |

No other panel renders a plan. Identifiers `PlanView` / `plan_view` / `PlanPanel` /
`plan_panel` / `PlanTab` / `plan_tab` / `PlanApproval` do **not** exist — the real names are
`PlanReviewPanel` / `plan_review` / `OpenRequest::PlanReview` / `HoldKind::Plan`.

---

## 2. Main plan-view file: `apps/desktop/src/views/panels/plan_review.rs`

### Struct (`plan_review.rs:42-60`)

```rust
pub struct PlanReviewPanel {
    session: SessionId,
    plan: String,
    /// Sends the operator's verdict to the engine; `None` for static/test views.
    commands: Option<UnboundedSender<Command>>,
    /// Whether a held approval is outstanding for this session (buttons are live).
    pending: bool,
    /// When the operator clicks **Reject**, this holds the reason input they fill in
    /// before the rejection is sent; `None` while the three top-level buttons show.
    reject_input: Option<Entity<InputState>>,
    focus_handle: FocusHandle,
    tab_panel: Option<WeakEntity<TabPanel>>,
    tab_menu: Option<super::TabMenu>,
    _subscription: Option<Task<()>>,
}
```

The plan payload is a plain `plan: String` (markdown text). It arrives via `new(...)`
(`plan_review.rs:66-106`) and is refreshed live: the panel folds the engine bus in
`apply_event` (`plan_review.rs:110-141`) — a fresh `EngineEvent::PlanProposed` for this
session re-arms the buttons and replaces `self.plan`:

```rust
EngineEvent::PlanProposed { session, plan } if session == watched => {
    self.plan = plan.clone();
    self.pending = true;
    self.reject_input = None;
    true
}
```

### Render (`plan_review.rs:511-577`)

The body renders the plan as markdown (note the comment already claims markdown, and it is —
just unstyled):

```rust
.child(
    // The plan, rendered as real markdown (headings/lists/code/bold)
    // via gpui-component's `TextView` so it reads as a document.
    div()
        .id("plan-body")
        .flex_1()
        .overflow_y_scroll()
        .rounded(theme::radius_md())
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::surface_raised())          // dark card…
        .px_4()
        .py_3()
        .child(TextView::markdown("plan-body-md", self.plan.clone())),  // …light-theme markdown → looks broken
)
.child(self.footer(cx))
```

**Why it looks broken/unstyled:** `TextView::markdown(id, text)` uses the crate default
`TextViewStyle` (`is_dark:false`, `HighlightTheme::default_light()`). There is no
`.style(...)` override and no `theme::markdown_style()` in the codebase. On the dark
`surface_raised()` card the code blocks render with a light background and headings/links use
light-mode colors → low-contrast, "washed out / unstyled" appearance. It is **populated and
parsed**, not raw and not empty.

### Footer / actions (`plan_review.rs:399-509`)

`footer()` shows the review state:
- `!self.pending` → muted "Review-only — no decision is currently awaited for this session."
  (this is what a **rehydrated** tab shows — see `dump` at `plan_review.rs:392-396`: the plan
  text is *not* persisted, only the session id, so a restored tab is non-pending until a
  fresh proposal arrives).
- pending → three pill buttons: **✓ Approve plan** (`approve`, native option 1 = accept+auto),
  **✦ Refine with ultraplan** (`refine`, native option 3), **✕ Reject** (`begin_reject` → inline
  reason field → `send_reject`).

Verdicts travel to the engine as `Command::ApproveAction` / `Command::DenyAction`
(`plan_review.rs:143-149, 294-300`), resolving the held hook; approve/refine additionally
drive Claude's post-`ExitPlanMode` continuation menu by polling the embedded terminal
(`select_native_option` / `is_continuation_menu`, `plan_review.rs:229-264, 316-319`).

---

## 3. Trigger & wiring — how plan data is captured and routed

There are **two** emission paths, both ending at `EngineEvent::PlanProposed` and the same
`PlanReviewPanel`:

### Path A — Hook keystone (primary, race-free) — Claude only
1. Claude calls `ExitPlanMode`; the `PreToolUse` hook forwards to the control server.
2. `crates/control/src/gate.rs:110-114` — `evaluate()` returns
   `GateDecision::Hold(HoldKind::Plan { plan: extract_plan(tool_input) })`. `extract_plan`
   (`gate.rs:155-158`) pulls `tool_input["plan"]` (the markdown). **This hold is
   phase-independent** — it fires for any adopted, non-paused session.
3. The server holds the call and notifies via `ApprovalNotifier`. The composition-root
   implementation `BusNotifier::approval_requested` (`apps/desktop/src/main.rs:632-656`)
   publishes:
   ```rust
   if let Some(plan) = plan {
       self.bus.publish(EngineEvent::PlanProposed {
           session: session.clone(),
           plan: plan.to_string(),
       });
   }
   ```
   (comment at `main.rs:626-627`: "Emitting `PlanProposed` here makes the hook (not the JSONL
   tail) the source of the plan text, so the plan tab opens with no polling race.")

### Path B — JSONL detection tail (fallback / observation) — Claude only
1. `crates/detection/src/jsonl.rs:211-217 extract_exit_plan` scans an assistant message for a
   `tool_use` block named `ExitPlanMode` and returns `input.plan`.
2. `crates/detection/src/lib.rs:295-301` — on a changed `Signal::Plan`, pushes
   `DetectionEvent::PlanProposed { session, plan }`.
3. `crates/engine/src/supervisor.rs:607-613` maps it to `EngineEvent::PlanProposed`.

### Convergence → the tab
Both paths hit the workspace bus subscriber in `workspace.rs:685-700`, which pushes an
Approval notification and emits the center-open request:
```rust
Ok(EngineEvent::PlanProposed { session, plan }) => {
    // …notification…
    let root = roots.get(&session).cloned().flatten();
    center.update(cx, |_center, cx| {
        cx.emit(OpenRequest::PlanReview { session, plan, root });
    });
}
```
`OpenRequest::PlanReview` (`center_requests.rs:31-35`) carries the plan markdown + the
emitting session's repo `root` (so the tab opens in *that* project's space). The workspace
then constructs `PlanReviewPanel::new(session, plan, pending=true, …)`
(`workspace.rs:2118-2121`, and the fresh-open site near `workspace.rs:694`/registration
`workspace.rs:321-327`).

### Is it tied to the "Plan" phase / ExitPlanMode / a hook / an agent message?
- **Tied to the `ExitPlanMode` tool call via the PreToolUse hook** (Path A) — the workflow
  "Plan" phase does **not** gate it (`gate.rs:110` keys purely off the tool name). It is also
  independently observable from the JSONL transcript (Path B).
- **It only appears when `ExitPlanMode` is emitted.** Since Claude emits that tool only in its
  native plan permission-mode, in **auto mode there is no plan view** (Claude never calls the
  tool). MoonlightCode does not synthesize a plan surface for auto-mode work. So: **populated
  during ExitPlanMode (typically Plan-mode sessions), NOT in auto mode.**

---

## 4. Gemini / AGY backend — does it reach the plan view?

**No.** Two reasons:

1. **No plan-tool mapping at the AGY edge.** `apps/desktop/src/agy_hook.rs:63-98 map_tool`
   translates AGY tool names onto Claude's vocabulary (`run_command`→`Bash`,
   `write_file`→`Write`, `edit_file`→`Edit`, `read_file`→`Read`, `grep_search`→`Grep`, …).
   There is **no arm producing `"ExitPlanMode"`**. AGY's native Plan Mode (`/plan`,
   `/planning`, `PlanMode` — documented in `.ai/handoffs/04-agy-gemini-integration-plan.md:109,114`)
   is a CLI mode, not a deniable `PreToolUse` tool call, so nothing arrives that `gate.rs:110`
   would recognize. Any unmapped AGY tool falls through the `other =>` passthrough
   (`agy_hook.rs:96`) with its raw name — never the literal `"ExitPlanMode"` — so
   `HoldKind::Plan` is never produced and `PlanProposed` is never emitted for AGY.
2. **JSONL detection is Claude-specific.** Path B reads `~/.claude` JSONL and matches
   `name == "ExitPlanMode"` (`crates/detection/src/jsonl.rs:215-216`); it does not parse
   Gemini/AGY conversation transcripts.

Consequently the AGY/Gemini path produces plans (its own Plan Mode) but they **do not reach
`PlanReviewPanel`**. Closing this gap needs either (a) an AGY→`ExitPlanMode` mapping in
`agy_hook::map_tool` (if AGY surfaces a plan as a hookable tool call) or (b) an AGY transcript
detector emitting `DetectionEvent::PlanProposed`.

---

## 5. Handoff docs — plan-view / plan-mode history

- **`00-status-and-tasks.md`** (canonical status):
  - `:1655` — "Plan-review **data + UI**: detection parses `ExitPlanMode` → `PlanProposed` →
    auto-opens 'Plan' center tab." (Path B origin.)
  - `:1632-1633` — Plan UI (`panels/plan_review.rs`): Approve/Reject buttons, folds the bus;
    "(T14 markdown render still open)".
  - `:1035-1036` (G3) — "render markdown properly (UI, T14)"; notes MCP is *not* the lever —
    "the plan already arrives as markdown via `ExitPlanMode`."
  - `:1737-1738` (**T14**) — "`panels/plan_review.rs` currently prints raw lines; render proper
    markdown … check for a `gpui-component` markdown widget first." (Superseded — markdown is
    now used; only styling remains, see handoff 01.)
  - `:721-739` — plan-review propagation fix (poll terminal for the continuation menu instead
    of a fixed delay); `:1279-1353` — 3-way plan gate + driving Claude's native continuation
    prompt (approve=1 / refine=3 / reject=hook-deny).
  - `:1225` — plan opens in the emitting session's project (`OpenRequest::PlanReview` carries
    `root`).
- **`01-t14-and-fullscreen-plan.md`** — the directly-relevant doc for the styling bug. States
  `TextView::markdown(...)` is called with the **default `TextViewStyle`** (`is_dark:false`,
  `HighlightTheme::default_light()`) at `plan_review.rs` (and `session_monitor.rs`), and
  proposes adding `theme::markdown_style() -> TextViewStyle` (dark) and applying
  `.style(theme::markdown_style())` at both call sites. **This fix is not present in the
  current tree** (no `markdown_style` in `views/theme.rs`; no `.style()` at
  `plan_review.rs:568`) — i.e. the plan renders with the wrong (light) theme.
- **`04-agy-gemini-integration-plan.md`** — AGY backend plan; `:103-124` phase↔AGY mode table
  maps **Plan phase → AGY `plan` mode** (`/plan`, `/planning`). Confirms AGY has native plan
  support but describes no `ExitPlanMode`-equivalent hook mapping (consistent with §4).
- **`05-frozen-phase-mcp-prompt-plan.md`** — reuses `panels/plan_review.rs` + `workspace.rs`
  as the pattern for the frozen-phase/MCP 4-way approval prompt UI (`:82,103`); not about the
  plan render itself.

---

## Fix summary (for whoever picks this up)
1. **Restyle (broken look):** add `theme::markdown_style()` (dark `TextViewStyle`) and apply
   `.style(theme::markdown_style())` to `TextView::markdown` at `plan_review.rs:568` (mirror
   for `session_monitor.rs` assistant markdown). This is the shipped-but-unfinished T14 from
   handoff 01.
2. **Auto-mode gap:** there is no plan surface when the agent works without `ExitPlanMode`
   (auto). If auto-mode plans are wanted, they need a new capture (e.g. a summary/plan message
   detector), not the existing `ExitPlanMode` hold.
3. **Gemini/AGY gap:** wire AGY's Plan Mode to `PlanProposed` — either map a plan tool in
   `agy_hook::map_tool` or add an AGY transcript detector — otherwise Gemini plans never open
   `PlanReviewPanel`.

---

## PROGRESS (2026-07-01)

### ✅ Workstream A — restyle (DONE, builds clean)
- Added `theme::markdown_style() -> TextViewStyle` (`views/theme.rs`): `is_dark:true`,
  `highlight_theme: HighlightTheme::default_dark()` (the real fix — code blocks were using the
  light syntax theme), a `paragraph_gap(rems(0.75))` and a heading ramp (`base * 1.4 / 1.2 / 1.05`
  via the `Mul<f32> for Pixels` impl). Imports `gpui_component::{highlighter::HighlightTheme,
  text::TextViewStyle}` + `gpui::rems`.
- `plan_review.rs` render: `.style(theme::markdown_style())` on the body `TextView`; body card
  bg changed `surface_raised()` → `surface_sunken()` (an inset "well") with `px_5/py_4`, so fenced
  code blocks — drawn on `cx.theme().muted` = `surface_raised()` — now stand out instead of
  colliding with the card. (Code-block bg is `muted` per gpui-component `text/node.rs:663`.)
- `session_monitor.rs:2527`: same `.style(theme::markdown_style())` on the assistant-message
  markdown (same T14 bug there).
- `cargo build -p moonlight-desktop`: 0 errors. `is_dark` is currently a *no-op* field in
  gpui-component (never read) — set for correctness/future; the effective fix is `default_dark()`
  + the sunken well.

### ✅ Workstream B — present_plan MCP verb (DONE, builds + tests green, 2026-07-02)
Auto-mode Claude **and** Gemini/AGY can now open the plan-review panel with a blocking hold:
- `crates/domain/src/trust.rs`: `McpVerb::PresentPlan` (tier `Observed`, `is_read_only`=true).
- `crates/control/src/gate.rs`: `pub const PRESENT_PLAN = "mcp__moonlight__present_plan"`; plan
  branch now holds on `EXIT_PLAN_MODE || PRESENT_PLAN` → reuses the exact `HoldKind::Plan` →
  `BusNotifier` → `PlanProposed` → panel path. Re-exported from the crate root. +2 unit tests.
- `crates/mcp-server/src/transport.rs`: `present_plan(plan: string)` rmcp `#[tool]` + `get_info`
  instructions. `lib.rs::danger_class` → `Safe` (hook does the gating; no server-side double-prompt).
- `apps/desktop/src/phase_verbs.rs`: trivial post-approval executor ack. `services.rs::verb_label`
  → `"present_plan"`.
- `apps/desktop/src/agy_hook.rs`: `map_tool` normalizes any AGY spelling ending in `present_plan`
  → canonical `PRESENT_PLAN` + `{plan}` (AGY gets the moonlight MCP host via
  `agy_setup::write_mcp_config`). +1 test. **Live-unverified:** AGY's actual MCP tool-call name in
  its PreToolUse payload (the `ends_with` match is defensive) and that AGY fires PreToolUse for MCP
  tools — needs a real Gemini session to confirm.
- **Panel safety (the native-menu hazard):** `plan_review.rs::select_native_option` now injects the
  digit **only when CC's continuation menu is actually detected on screen**. A `present_plan`/auto/
  Gemini plan never renders that menu, so approve no longer types a stray `1↵` into the session; the
  held hook is still resolved (approve=allow). Chose this 1-line fix over threading a `native` origin
  through the `PlanProposed` event + `ApprovalNotifier` trait (which would touch the trust boundary
  in ~8 sites). Trade-off: if CC rewords its menu so the heuristic misses, approve won't auto-pick
  option 1 — the operator picks it manually. Acceptable + recoverable.

### ⏳ Remaining: make auto-mode *automatically* surface plans + Workstream C detector

`present_plan` works, but the agent must **choose** to call it. Two ways to make auto mode "just
work" (open question for operator):
- **Prompt nudge (lower risk):** instruct auto-mode agents (Claude via `--append-system-prompt`;
  Gemini via its system prompt) to call `present_plan` before starting multi-step work. Deterministic,
  no false positives.
- **Detector (Workstream C, best-effort, non-blocking):** `crates/detection` recognizes a plan-shaped
  assistant message when neither `ExitPlanMode` nor `present_plan` fired → `DetectionEvent::PlanProposed`
  (review-only, can't hold post-hoc). Dedup vs the verb. Heuristic → false-positive risk. Gemini
  transcript detection needs AGY's transcript format first.

### Original spec (kept for reference)

Decisions locked with operator: **MCP verb (primary, blocking hold) + detector fallback**;
**blocking approval hold**.

**Route: reuse the hook-hold path (NOT the verb-approval gate).** Rationale, verified in code:
- `PendingApprovals` (`crates/control/src/pending.rs`) is keyed **by session**; the plan panel's
  `Command::ApproveAction { session }` / `DenyAction` resolve that per-session oneshot
  (`main.rs:481`). So any hold registered for the session is resolved by the panel buttons.
- The hook path already publishes the plan markdown to open the panel:
  `gate.rs` `Hold(HoldKind::Plan{plan})` → `ApprovalNotifier::approval_requested(session, what,
  plan=Some, mcp_tool)` (`main.rs:632`) → `EngineEvent::PlanProposed{plan}` → panel.
- The verb-approval gate (`ActorService::run` → `self.approval.request(session, what)`,
  `mcp-server/src/lib.rs:231`) can block but its `ApprovalGate::request(session, what)` signature
  carries **no plan markdown**, so it cannot open the panel without new plumbing. Hook path wins.

**B1 — `present_plan` as an advertised verb + gate hold (Claude auto-mode):**
1. `crates/domain/src/trust.rs`: add `McpVerb::PresentPlan`. `min_autonomous_tier` → `Observed`
   (the *hold* does the gating, not the tier). `is_read_only()` → true (no project side effect;
   must pass the frozen-phase gate so it works in Plan phase too). `danger_class` (mcp-server
   `lib.rs:297`) → `Safe` (so `ActorService::run` doesn't *double*-prompt after the hook already
   held+approved).
2. Advertise it in the embedded server's tool list + map name→verb (find the `McpVerb` name
   table — `mcp-server/src/adapters.rs` / wherever `"request_phase"`→`RequestPhase` lives). Tool
   schema: `present_plan(plan: string /*markdown*/, title?: string)`. Description must tell the
   agent to call it to surface a plan for operator review **in any mode** (esp. auto), instead of
   only native plan-mode.
3. Trivial verb executor (the post-approval execution): return `"Plan presented."`. It only runs
   *after* the hook hold is approved (on reject the hook denies and the tool never reaches the
   server).
4. `crates/control/src/gate.rs`: add `pub const PRESENT_PLAN: &str = "mcp__moonlight__present_plan";`
   and extend the plan branch: `if tool_name == EXIT_PLAN_MODE || tool_name == PRESENT_PLAN {
   Hold(HoldKind::Plan { plan: extract_plan(tool_input) }) }`. Add a unit test mirroring
   `exit_plan_mode_holds_for_plan_approval`. NOTE `extract_plan` reads `tool_input["plan"]` — keep
   the verb's arg name `plan`.

**B2 — panel must NOT drive Claude's native menu for non-native plans (REQUIRED before B1 ships):**
- `plan_review.rs` `approve()`/`refine()` call `select_native_option()` which polls the session
  terminal for CC's post-`ExitPlanMode` continuation menu and injects `1`/`3`. For a
  `present_plan`/auto/Gemini hold there is **no** such menu → it would wait 6s then inject a stray
  `1\r` into the session (a spurious user turn). Fix by threading a plan **origin**:
  - Add `native: bool` to `EngineEvent::PlanProposed` (`engine/src/lib.rs:56`). Set `true` at the
    two existing emitters (hook path in `main.rs` when the held tool is `ExitPlanMode`; detection
    path in `supervisor.rs:607`) and `false` for `present_plan`. To know the tool at the hook
    emitter, pass the held tool name through `ApprovalNotifier::approval_requested` (today
    `mcp_tool` is only set for `McpAuthorize`; either always pass the tool name or add a
    `native`/`plan_kind` param — the control server has it where it builds `HoldKind::Plan`).
  - Thread `native` → `OpenRequest::PlanReview` (`center_requests.rs:31`) → `PlanReviewPanel::new`
    (`workspace.rs:2118`) → store on the panel; `apply_event` also updates it on a fresh
    `PlanProposed`. Only call `select_native_option` when `native`.
  - Detection stays `native:true` (it observes a real `ExitPlanMode`), preserving today's behavior.
- Alternative considered & rejected: make `select_native_option` inject *only when the menu is
  detected*. Simpler (no event change) but it deletes the intentional "blind fallback" that
  guards against CC menu-wording drift for the real native flow. Thread origin instead.

**B3 — Gemini/AGY:**
- `agy_hook.rs::map_tool`: add an arm mapping AGY's plan-presentation tool → `("mcp__moonlight__present_plan",
  json!({ "plan": <markdown> }))`, pulling the markdown from AGY's arg key. **UNVERIFIED:** AGY's
  actual plan tool name + arg key, and whether AGY's launch injects `--mcp-config` so Gemini even
  sees the moonlight verb (memory says MCP is wired at all launch sites — confirm the AGY site in
  `agy_setup.rs`). If AGY can't call the verb, fall back to the C detector for Gemini.

**C — detector fallback (best-effort, non-blocking by nature):**
- `crates/detection`: recognize a plan-shaped assistant message when neither `ExitPlanMode` nor
  `present_plan` fired, emit `DetectionEvent::PlanProposed` (→ `native:true` today, but it can't
  hold post-hoc, so it surfaces review-only / non-pending). Dedup so it never double-opens with
  the verb. Gemini transcript detection needs AGY's transcript format first — scope as follow-up.

**Verify:** `rtk cargo build/clippy/test`; then manual — fire `present_plan` from an auto-mode
Claude session and (if wired) a Gemini session: tab opens, is styled, Approve/Reject resolves the
hold, and **no** stray keystroke lands in the terminal.

**Why B/C is checkpointed, not auto-landed:** it edits the approval/gate trust boundary
(`gate.rs`, `PendingApprovals`, the notifier) and a GPUI app can't be visually verified headlessly
— worth an operator review before touching the security-relevant hold path.
