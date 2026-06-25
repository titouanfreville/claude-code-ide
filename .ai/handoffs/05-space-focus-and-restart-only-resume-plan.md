# 05 — Two small fixes: space-return focus + restart-only auto-resume

> **STATUS: IMPLEMENTED** (2026-06-25). Builds clean, 236 desktop tests pass.
> Key mechanism choices that differ slightly from the plan below:
> - **Issue 1 recording** uses the existing `active_context` observer in `Workspace`
>   (fires on any session-tab activation) rather than a new focus hook — when an
>   `ActiveContext::Session` becomes frontmost it's recorded against `current_space`.
>   Mounting is made order-aware via `Workspace::incoming_ordered` (remembered tab
>   added last so `add_panel`'s "last-added wins" lands it frontmost). Persisted via
>   `SpaceTabs::active_tab` (sidecar `OPEN_TABS_VERSION` bumped 1→2; `#[serde(default)]`
>   so old files still load).
> - **Issue 2** keeps the `Incomplete`-alert trigger but gates it behind a one-shot
>   `resume_armed` flag set ONLY on the restart-restore path (`build_center_panel`'s
>   new `restoring` arg → `SessionMonitor::arm_restore_resume`, gated on the project's
>   `auto_resume` opt-in). Live stalls are never armed, so they no longer fire.


Two unrelated usability bugs. Both are in `apps/desktop/src/views/`.

---

## ISSUE 1 — Returning to a space focuses the wrong (rightmost) session tab

### Symptom
Switching back into a project space activates whatever tab lands last in the
mount order (perceived as "the rightmost session"), not the session the operator
was last looking at in that space.

### Root cause
gpui-component's `TabPanel::add_panel` always makes the **just-added** panel active
(`add_panel` → `add_panel_with_active(.., active=true)` → `set_active_ix(len-1)`,
`tab_panel.rs:245-283`). So whichever panel is mounted **last** wins focus.

Two mount sites feed it:
- `switch_space` (`workspace.rs:1530-1548`): builds `incoming` from
  `space_panels.get(&new).map(|m| m.values().cloned().collect())` — a
  **`HashMap` value iteration**, i.e. arbitrary order. The last one out of the
  HashMap becomes active.
- `restore_open_tabs` eager branch (`workspace.rs:1465-1476`): iterates
  `entry.tabs` (an *ordered* Vec) so the last open tab wins at startup.

There is no per-space record of "which session tab was frontmost," so nothing can
restore it. `ProjectSpace` only tracks **one** globally-focused session
(`session: Option<SessionId>`, `project_space.rs:114`), not a per-space last-focus.

### Fix
Record the last-active session tab **per space**, then activate it after a space's
tabs are mounted.

1. **Track per-space active tab.** Add to `Workspace`:
   `space_active_tab: HashMap<Option<SpaceId>, String>` (value = tab dedup key,
   e.g. `"session:<id>"`).
   Populate it from the existing **focus observer** (`workspace.rs:790`): when
   `focus.read(cx).session` resolves to a space, write
   `space_active_tab[space] = "session:<id>"`. (Focusing a session already
   re-roots to its space via `ProjectSpace::focus`, so the signal is there.)
   The `session:<id>` key matches `OpenRequest::key` / `space_panels` keys.

2. **Activate the remembered tab on mount.** Make mounting order-aware: after
   adding a space's `incoming` panels, ensure the remembered tab is the active one.
   Cleanest: a small helper that, given the space's panel map + remembered key,
   adds panels with the target **last** (so `add_panel`'s "last wins" lands on it),
   falling back to current behavior when there is no record. Apply in both:
   - `switch_space` (`incoming` loop), and
   - `restore_open_tabs` eager branch.

3. **Persist across restart (so "come back" survives a relaunch).** Add
   `active_tab: Option<String>` to `open_tabs::SpaceTabs` (`open_tabs.rs:37`,
   bump `OPEN_TABS_VERSION` to 2 — old files are version-gated out, harmless).
   Write it from `space_active_tab` when saving; seed `space_active_tab` from it
   on `restore_open_tabs`.

### Design note — what "latest active" means
This restores the **last-focused** session tab in that space (the one you were
looking at when you left). That matches "come back to where I was." (Alternative —
the session with the most recent CC output — would need activity timestamps we
don't track; not pursued unless you want that instead.)

### Files
- `apps/desktop/src/views/workspace.rs` — new field; focus-observer write;
  order-aware mount in `switch_space` + `restore_open_tabs`.
- `apps/desktop/src/views/open_tabs.rs` — `active_tab` field + version bump.

---

## ISSUE 2 — Auto-resume prompt fires on every natural stall, not just IDE restart

### Symptom
The auto-resume "continue" prompt is injected whenever a managed session goes
quiet mid-turn during normal operation. It should fire **only on IDE restart**
(to nudge a restored session to pick up where it left off).

### Root cause
`SessionMonitor::maybe_auto_resume` (`session_monitor.rs:427-462`) is wired into
the live engine-event loop (`session_monitor.rs:199`) and edge-triggers on the
transition **into the `Incomplete` attention alert** — i.e. any time a live
session stalls past its working window. That is the "stopped naturally for too
long" behavior the user wants gone.

### Fix
Move the trigger from "live stall alert" to "managed session re-resumed during
startup restore."

1. **Remove the live trigger.** Drop the `this.maybe_auto_resume(&event, cx)` call
   at `session_monitor.rs:199` (and the now-unused alert-watching method, or
   repurpose it). The `last_alert` field stays only if still needed elsewhere.

2. **Inject on restore instead.** When a **managed** session is rebuilt as part of
   restoring previously-open tabs **and** its project opted into auto-resume, arm a
   one-shot injection of `AUTO_RESUME_PROMPT` into its freshly-resumed terminal.
   - Restart is distinguishable: the restore-only paths are
     `restore_open_tabs` (eager) and `realize_pending` (lazy first-switch), both
     via `build_center_panel` (`workspace.rs:2059`). The live "operator opens a
     tab" path and "operator launches a new session" path are separate, so they
     won't be affected.
   - Plumb a `restoring: bool` (or a dedicated builder flag) into the managed
     branch of `build_center_panel` so only restore mounts arm the injection;
     check `deps.focus.read(cx).project_auto_resume(&root)` there.
   - Delivery: arm a one-shot terminal injection that fires once CC is ready
     (mirror the auto-compact `TerminalPanel::arm_injection` pattern rather than a
     blind immediate `send_text`, since `claude --resume` needs a moment to boot).

### Files
- `apps/desktop/src/views/panels/session_monitor.rs` — remove live trigger; add a
  restore-time "arm resume injection" entry point.
- `apps/desktop/src/views/workspace.rs` — `build_center_panel` managed branch:
  pass the restoring flag + opt-in check; restore call sites pass `true`.

---

## Test / verify
- `rtk cargo build -p moonlight-desktop`
- `rtk cargo test -p moonlight-desktop` (open_tabs round-trip incl. new field;
  add a switch_space focus-ordering unit test if feasible).
- Manual: (1) focus session A in a space, switch away, switch back → A is active,
  not the rightmost tab. (2) With auto-resume on, let a live session stall → no
  prompt injected. Restart the IDE → restored managed session gets the continue
  prompt once.
