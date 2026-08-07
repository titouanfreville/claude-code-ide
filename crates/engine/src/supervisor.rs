//! The session supervisor — owns the live fleet and is the sole component that
//! drives sessions through the [`ControlPort`] and reacts to [`DetectionEvent`]s.
//!
//! Flow: the UI emits [`Command`]s inbound; detection adapters feed
//! [`DetectionEvent`]s inbound. The supervisor mutates session state, issues the
//! matching control-port call, and publishes [`EngineEvent`] **facts** on the
//! [`EventBus`]. It never mutates UI state, never touches a concrete adapter, and
//! never panics: port errors are logged via `tracing` and surfaced as facts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use moonlight_domain::audit::{AuditAction, AuditEntry};
use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::control::ControlPort;
use moonlight_domain::ports::detection::DetectionEvent;
use moonlight_domain::ports::store::{ManagedSessionStore, ManagedStateUpdate};
use moonlight_domain::review::{Feedback, FeedbackOrigin};
use moonlight_domain::session::{AttentionKind, Mode, Session, SessionStatus};
use moonlight_domain::trust::TrustTier;

use crate::bus::EventBus;
use crate::{Command, EngineEvent};

/// Owns the supervised fleet and orchestrates control + detection.
pub struct SessionSupervisor {
    fleet: HashMap<SessionId, Session>,
    bus: EventBus,
    control: Arc<dyn ControlPort>,
    /// Durable store of managed-session state + the audit log. `None` keeps the
    /// supervisor pure (tests / headless); when present, managed-session state is
    /// refreshed on change (UPDATE-only, so only sessions the app launched are
    /// persisted) and autonomous actions are appended to the audit log.
    store: Option<Arc<dyn ManagedSessionStore>>,
    /// Monotonic counter feeding time-ordered audit-entry ids.
    audit_seq: u64,
}

impl SessionSupervisor {
    /// Construct a supervisor over an injected control port and event bus, with no
    /// persistence (pure — used by tests and any headless path).
    pub fn new(control: Arc<dyn ControlPort>, bus: EventBus) -> Self {
        Self::with_store(control, bus, None)
    }

    /// Construct a supervisor that also persists managed-session state + audit to
    /// the injected store (the composition root wires the concrete SQLite store).
    pub fn with_store(
        control: Arc<dyn ControlPort>,
        bus: EventBus,
        store: Option<Arc<dyn ManagedSessionStore>>,
    ) -> Self {
        Self {
            fleet: HashMap::new(),
            bus,
            control,
            store,
            audit_seq: 0,
        }
    }

    /// Rehydrate the in-memory fleet from the durable store (FR42). Without this the
    /// supervisor boots with an empty fleet, and the cockpit grid — which only grows
    /// from live `SessionUpserted` deltas — shows nothing until a session is launched
    /// or shows fresh activity, so previously-managed sessions vanish across a restart
    /// even though their records survive on disk.
    ///
    /// For each persisted record it seeds an **idle** [`Session`] (the resting state;
    /// detection promotes it to `Running` if it is actually live) and publishes a
    /// `SessionUpserted` so every subscriber — fleet grid, gate read-model, policy
    /// view — picks up the restored fleet. **Idempotent**: a session already present
    /// in the live fleet is skipped, never clobbering fresher in-memory state. Trust
    /// tier isn't persisted, so it restores at the default-deny `Observed` tier (the
    /// operator re-confirms trust). No-op without a store. Logged, never fatal.
    pub fn hydrate_from_store(&mut self) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let managed = match store.all_managed() {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(error = %err, "fleet rehydration failed to read the store");
                return;
            }
        };
        let mut restored = 0usize;
        for m in managed {
            if self.fleet.contains_key(&m.id) {
                continue;
            }
            let session = m.to_session();
            self.fleet.insert(m.id.clone(), session.clone());
            self.bus.publish(EngineEvent::SessionUpserted { session });
            restored += 1;
        }
        tracing::info!(
            restored,
            fleet = self.fleet.len(),
            "fleet rehydrated from store"
        );
    }

    /// Refresh the persisted state of a *managed* session (UPDATE-only — a no-op if
    /// the session was never launched/taken-over by the app, keeping the managed
    /// table free of merely-observed sessions). Logged, never fatal.
    fn persist(&self, s: &Session) {
        let Some(store) = &self.store else { return };
        let update = ManagedStateUpdate {
            id: s.id.clone(),
            title: s.title.clone(),
            phase: s.phase,
            mode: s.mode,
            trust_tier: s.trust_tier,
            adopted: s.adopted,
            paused: s.paused,
            phase_pinned: s.phase_pinned,
            hidden: s.hidden,
            last_seen: s.last_activity,
        };
        if let Err(err) = store.update_managed_state(&update) {
            tracing::warn!(session = %s.id, error = %err, "persist managed state failed");
        }
    }

    /// Append one audit entry for an autonomous action (no-op without a store).
    /// The id is time-ordered (`<millis>-<seq>`) so the append-only log sorts by
    /// occurrence even within the same millisecond.
    fn audit(&mut self, session: &SessionId, action: AuditAction, revertible: bool) {
        let Some(store) = self.store.clone() else {
            return;
        };
        self.audit_seq += 1;
        let at = now();
        let entry = AuditEntry {
            id: format!("{:013}-{:06}", at.as_millis(), self.audit_seq),
            session_id: session.clone(),
            at,
            action,
            revertible,
        };
        if let Err(err) = store.append_audit(&entry) {
            tracing::warn!(session = %session, error = %err, "append audit failed");
        }
    }

    /// Read-only view of a single session (for UI snapshots / tests).
    pub fn session(&self, id: &SessionId) -> Option<&Session> {
        self.fleet.get(id)
    }

    /// Number of sessions currently supervised.
    pub fn len(&self) -> usize {
        self.fleet.len()
    }

    /// Whether the fleet is empty.
    pub fn is_empty(&self) -> bool {
        self.fleet.is_empty()
    }

    // ---- Inbound: operator commands -------------------------------------

    /// Apply one operator [`Command`]: drive the control port, mutate state, and
    /// publish the resulting fact(s). Port failures are logged and surfaced as
    /// [`EngineEvent::AuditAppended`] — never propagated as a panic.
    pub async fn handle_command(&mut self, command: Command) {
        match command {
            Command::SpawnSession {
                prompt,
                attached_path,
            } => self.spawn_session(prompt, attached_path).await,
            // Per-hunk rejection delivers through the `ControlPort` like any other
            // feedback (Option C): the `SteerControl` adapter queues it for the UI,
            // which writes it into the session's embedded terminal. `inject` audits
            // on success.
            Command::RejectHunk { feedback } => {
                self.inject(feedback, "hunk rejected").await;
            }
            Command::DenyAction { session, reason } => {
                let feedback = Feedback {
                    session_id: session,
                    message: reason,
                    origin: FeedbackOrigin::DangerZoneDenial,
                };
                self.inject(feedback, "action denied").await;
            }
            Command::Steer { session, message } => {
                let feedback = Feedback {
                    session_id: session,
                    message,
                    origin: FeedbackOrigin::OperatorSteer,
                };
                self.inject(feedback, "steered").await;
            }
            Command::SubmitReview { session, message } => {
                let feedback = Feedback {
                    session_id: session,
                    message,
                    origin: FeedbackOrigin::ReviewComment,
                };
                self.inject(feedback, "review submitted").await;
            }
            Command::ApproveAction { session } => self.approve_action(session).await,
            Command::ApprovePlan { session } => self.approve_plan(session).await,
            Command::ToggleAdoption { session } => self.toggle_adoption(session),
            Command::SetAdopted { session, adopted } => self.set_adopted(session, adopted),
            Command::SetTrust { session, tier } => self.set_trust(session, tier),
            Command::TogglePause { session } => self.toggle_pause(session),
            Command::SetHidden { session, hidden } => self.set_hidden(session, hidden),
            // A manual phase pick is an operator override → **pin** it (A ≫ B), so
            // auto-advance and detection reconcile leave it alone until released.
            Command::SetPhase { session, phase } => self.apply_phase(session, phase, true).await,
            Command::AdvancePhase { session } => self.advance_phase(session).await,
            Command::SetPhasePinned { session, pinned } => self.set_phase_pinned(session, pinned),
            Command::RehydrateFleet => self.hydrate_from_store(),
            Command::ForgetSession { session } => self.forget_session(session),
            Command::FlagSession { session, alert } => self.flag_session(session, alert),
            // Handled by the composition-root command router (runtime overlay + config
            // persist) before reaching the supervisor; arm kept for exhaustiveness.
            Command::AuthorizeAlwaysTool { .. } => {
                tracing::debug!(
                    "AuthorizeAlwaysTool reached supervisor — expected router to consume it"
                );
            }
        }
    }

    /// Raise or clear a session's attention overlay (the ⚠ signal). Transient — not
    /// persisted (like status); the supervisor just republishes it on the bus for the
    /// UI to fold. Stamps activity so a self-reporting session counts as alive.
    fn flag_session(&mut self, session: SessionId, alert: Option<AttentionKind>) {
        if let Some(s) = self.fleet.get_mut(&session) {
            s.last_activity = now();
        }
        self.bus
            .publish(EngineEvent::SessionAlert { session, alert });
    }

    /// Forget a session: drop it from the live fleet, delete its managed record, and
    /// announce the removal so the grid drops its tile. The transcript on disk is
    /// untouched (the conversation history survives; only our tracking goes).
    fn forget_session(&mut self, session: SessionId) {
        self.fleet.remove(&session);
        if let Some(store) = &self.store {
            if let Err(err) = store.remove_managed(&session) {
                tracing::warn!(session = %session, error = %err, "remove managed record failed");
            }
        }
        tracing::info!(session = %session, "session forgotten");
        self.bus.publish(EngineEvent::SessionRemoved { session });
    }

    /// Flip a session's adoption (operator opt-in to governance) and republish the
    /// full row so the UI and control gate both see the new state.
    fn toggle_adoption(&mut self, session: SessionId) {
        let Some(s) = self.fleet.get_mut(&session) else {
            tracing::warn!(session = %session, "ToggleAdoption for unknown session");
            return;
        };
        s.adopted = !s.adopted;
        s.last_activity = now();
        let adopted = s.adopted;
        let upserted = s.clone();
        tracing::info!(session = %session, adopted, "adoption toggled");
        self.persist(&upserted);
        self.bus
            .publish(EngineEvent::SessionUpserted { session: upserted });
    }

    /// Set a session's adoption to an explicit value (idempotent — unlike
    /// [`toggle_adoption`]). Used to **auto-adopt** sessions the app creates or
    /// imports, so the PDP gates them immediately without a manual opt-in. No-op if
    /// the value is unchanged or the session isn't tracked yet (a freshly-launched
    /// session is adopted instead via its store record when detection discovers it).
    fn set_adopted(&mut self, session: SessionId, adopted: bool) {
        let Some(s) = self.fleet.get_mut(&session) else {
            tracing::debug!(session = %session, "SetAdopted for untracked session (ignored)");
            return;
        };
        if s.adopted == adopted {
            return;
        }
        s.adopted = adopted;
        s.last_activity = now();
        let upserted = s.clone();
        tracing::info!(session = %session, adopted, "adoption set");
        self.persist(&upserted);
        self.bus
            .publish(EngineEvent::SessionUpserted { session: upserted });
    }

    /// Set a session's trust tier (operator override), persist it, and republish the
    /// row so the control gate's PDP sees the new tier. No-op if unchanged or untracked.
    /// Persisting (via [`Self::persist`], UPDATE-only) is what makes a "trust this
    /// session" decision survive restart/reset instead of reseeding to `Observed`.
    fn set_trust(&mut self, session: SessionId, tier: TrustTier) {
        let Some(s) = self.fleet.get_mut(&session) else {
            tracing::debug!(session = %session, "SetTrust for untracked session (ignored)");
            return;
        };
        if s.trust_tier == tier {
            return;
        }
        s.trust_tier = tier;
        s.last_activity = now();
        let upserted = s.clone();
        tracing::info!(session = %session, ?tier, "trust tier set");
        self.persist(&upserted);
        self.bus
            .publish(EngineEvent::SessionUpserted { session: upserted });
    }

    /// Flip a session's pause (operator safety halt) and republish the full row so
    /// the control gate denies/allows its tools accordingly.
    fn toggle_pause(&mut self, session: SessionId) {
        let Some(s) = self.fleet.get_mut(&session) else {
            tracing::warn!(session = %session, "TogglePause for unknown session");
            return;
        };
        s.paused = !s.paused;
        s.last_activity = now();
        let paused = s.paused;
        let upserted = s.clone();
        tracing::info!(session = %session, paused, "pause toggled");
        self.persist(&upserted);
        self.bus
            .publish(EngineEvent::SessionUpserted { session: upserted });
    }

    /// Soft-hide / unhide a session — mask it from the default fleet view (or restore
    /// it). Republishes the full row so the grid re-renders with the new `hidden`
    /// flag. Deliberately does **not** bump `last_activity`: hiding isn't activity, so
    /// an unhidden session keeps its real place in the recency order.
    fn set_hidden(&mut self, session: SessionId, hidden: bool) {
        let Some(s) = self.fleet.get_mut(&session) else {
            tracing::warn!(session = %session, "SetHidden for unknown session");
            return;
        };
        s.hidden = hidden;
        let upserted = s.clone();
        tracing::info!(session = %session, hidden, "hidden toggled");
        self.persist(&upserted);
        self.bus
            .publish(EngineEvent::SessionUpserted { session: upserted });
    }

    async fn spawn_session(&mut self, prompt: String, attached_path: Option<String>) {
        let starting_phase = Phase::Plan;
        match self
            .control
            .spawn(&prompt, attached_path.as_deref(), starting_phase)
            .await
        {
            Ok(id) => {
                let session = Session {
                    id: id.clone(),
                    title: None,
                    status: SessionStatus::Running,
                    phase: starting_phase,
                    // Default-deny posture: new sessions start in the read-only Plan
                    // phase (CC `auto`, project writes denied by the PDP) and untrusted.
                    mode: starting_phase.operator_mode(),
                    trust_tier: TrustTier::Observed,
                    attached_path,
                    pinned: false,
                    adopted: false,
                    paused: false,
                    phase_pinned: false,
                    hidden: false,
                    last_activity: now(),
                };
                self.fleet.insert(id.clone(), session.clone());
                tracing::info!(session = %id, "session spawned");
                self.bus.publish(EngineEvent::SessionUpserted { session });
            }
            Err(err) => {
                // No SessionId exists yet, so there is nothing to attach a fact to;
                // log the failure for the operator/diagnostics.
                tracing::error!(error = %err, "spawn failed");
            }
        }
    }

    /// The single workflow-phase transition. Sets the phase the PDP gates on + the
    /// derived mode + the **pin** flag (`pinned` = is this an operator override?),
    /// persists, audits, tells the agent the aim when entering Plan, fires
    /// `ReviewReady` when entering Review (opens the code-review gate), and
    /// republishes the full session row. Callers choose `pinned`: a manual
    /// [`Command::SetPhase`] pins (A ≫ B); auto-advance and [`advance_phase`] don't.
    async fn apply_phase(&mut self, session: SessionId, phase: Phase, pinned: bool) {
        let Some(s) = self.fleet.get_mut(&session) else {
            tracing::warn!(session = %session, "apply_phase for unknown session");
            return;
        };
        let previous = s.phase;
        // Phase is our authority; the displayed mode is derived from it so the two
        // can never drift. Every phase is CC `auto` and they differ only in our write
        // policy, so switching between them needs no CC interaction — just the phase
        // the PDP gates on.
        s.phase = phase;
        s.mode = phase.operator_mode();
        s.phase_pinned = pinned;
        s.last_activity = now();
        let updated = s.clone();
        self.persist(&updated);
        self.audit(&session, AuditAction::PhaseChanged { to: phase }, false);

        // Full row so the UI + gate fold pick up phase, mode, and the pin together
        // (a thin `PhaseTransitioned` wouldn't carry the pin).
        self.bus
            .publish(EngineEvent::SessionUpserted { session: updated });

        // *Entering* Plan tells the agent the aim. CC runs `auto` in every phase, so
        // nothing about the phase itself makes it plan — `PLAN_AIM` is what does. A
        // fresh session gets the same text from its launch system prompt; this covers
        // the session that enters Plan mid-conversation. Injection is a no-op on
        // observe-only control (logged quietly) — the PDP read-only enforcement is
        // what guarantees safety, so a failed nudge degrades cleanly.
        if phase == Phase::Plan && previous != Phase::Plan {
            let feedback = Feedback {
                session_id: session.clone(),
                message: moonlight_domain::phase::PLAN_AIM.to_string(),
                origin: FeedbackOrigin::PhaseAim,
            };
            self.inject(feedback, "plan aim").await;
        }
        // Entering Review opens the code-review gate — replaces the old FR13
        // always-on-Done announcement; now ReviewReady fires when the workflow
        // actually reaches Review.
        if phase == Phase::Review {
            self.bus.publish(EngineEvent::ReviewReady { session });
        }
    }

    /// Operator-confirmed advance to the next workflow phase ([`Phase::next`],
    /// cyclic: Commit→Plan), returning the session to **auto** mode (clears the
    /// pin). Drives the Test/Commit advance buttons, code-review Approve
    /// (Review→Commit), and the pinned-advance approval.
    async fn advance_phase(&mut self, session: SessionId) {
        let Some(s) = self.fleet.get(&session) else {
            tracing::warn!(session = %session, "AdvancePhase for unknown session");
            return;
        };
        let next = s.phase.next();
        self.apply_phase(session, next, false).await;
    }

    /// Pin / unpin a session's phase (explicit lock/unlock affordance). Pinning
    /// freezes auto-advance and shields the phase from detection reconcile.
    fn set_phase_pinned(&mut self, session: SessionId, pinned: bool) {
        let Some(s) = self.fleet.get_mut(&session) else {
            tracing::warn!(session = %session, "SetPhasePinned for unknown session");
            return;
        };
        if s.phase_pinned == pinned {
            return; // idempotent — no event for a no-op
        }
        s.phase_pinned = pinned;
        s.last_activity = now();
        let updated = s.clone();
        self.persist(&updated);
        self.bus
            .publish(EngineEvent::SessionUpserted { session: updated });
    }

    async fn inject(&mut self, feedback: Feedback, what: &str) {
        let session = feedback.session_id.clone();
        if let Some(s) = self.fleet.get_mut(&session) {
            s.last_activity = now();
        }
        match self.control.inject_feedback(&feedback).await {
            Ok(()) => {
                tracing::info!(session = %session, origin = ?feedback.origin, "feedback injected");
                self.audit(
                    &session,
                    AuditAction::FeedbackInjected {
                        message: feedback.message.clone(),
                    },
                    false,
                );
                self.bus.publish(EngineEvent::AuditAppended {
                    session,
                    summary: format!("{what}: {}", feedback.message),
                });
            }
            Err(err) => self.surface_control_error(&session, "inject_feedback", err),
        }
    }

    async fn approve_action(&mut self, session: SessionId) {
        match self.control.resume(&session).await {
            Ok(()) => {
                let status = SessionStatus::Running;
                if let Some(s) = self.fleet.get_mut(&session) {
                    s.status = status;
                    s.last_activity = now();
                }
                tracing::info!(session = %session, "action approved, resuming");
                self.bus
                    .publish(EngineEvent::SessionStateChanged { session, status });
            }
            Err(err) => self.surface_control_error(&session, "resume", err),
        }
    }

    /// A plan was approved: hand the session off to [`Phase::AutoImplement`] so it can
    /// implement what was just approved. Unblocking the held call is
    /// [`Command::ApproveAction`]'s job (see [`Command::ApprovePlan`]).
    ///
    /// This is the **plan keystone**. It used to happen implicitly — CC left its
    /// native plan mode, detection observed `auto`, and `PhaseObserved` flipped the
    /// phase. Plan now runs CC in `auto` like every other phase, so that signal is
    /// gone and the advance has to be explicit. A session that is *not* in Plan (a
    /// native plan proposed from, say, Review) keeps its phase — that is the
    /// operator's business. A **pinned** phase is never moved silently: it raises
    /// `PhaseAdvanceRequested` instead (A ≫ B), same as the done-path.
    async fn approve_plan(&mut self, session: SessionId) {
        let Some(s) = self.fleet.get(&session) else {
            tracing::warn!(session = %session, "ApprovePlan for unknown session");
            return;
        };
        if s.phase != Phase::Plan {
            return;
        }
        if s.phase_pinned {
            self.bus.publish(EngineEvent::PhaseAdvanceRequested {
                session,
                to: Phase::Plan.next(),
            });
        } else {
            self.apply_phase(session, Phase::Plan.next(), false).await;
        }
    }

    // ---- Inbound: detection ---------------------------------------------

    /// React to one observed [`DetectionEvent`]: update the session's status or
    /// phase and publish the corresponding fact. A "done" status triggers the
    /// FR13 auto-revert to [`Phase::Plan`] and a [`EngineEvent::ReviewReady`].
    pub async fn on_detection(&mut self, event: DetectionEvent) {
        match event {
            DetectionEvent::StatusChanged { session, status } => {
                self.apply_status(session, status).await;
            }
            // The detector observed an abnormal end (stall) or its recovery — surface
            // it as the attention overlay (transient; mirrors the self-report path).
            DetectionEvent::Alert { session, alert } => self.flag_session(session, alert),
            DetectionEvent::PhaseObserved { session, phase } => {
                let Some(s) = self.fleet.get_mut(&session) else {
                    tracing::warn!(session = %session, "PhaseObserved for unknown session");
                    return;
                };
                // Detection can only see CC's **plan vs non-plan** mode from the
                // transcript (`phase_from_mode` emits Plan or AutoImplement). It must
                // NOT clobber our richer phase: **every** phase now runs CC in `auto`,
                // so observing `auto` says nothing at all — it is consistent with
                // whichever phase the operator/engine already chose, Plan included.
                // The one signal left is the operator shift-tabbing a live session into
                // CC's own plan mode, which we adopt as Phase::Plan.
                let reconciled = if s.phase_pinned {
                    // Operator pinned the phase — detection must not clobber it (A ≫ B).
                    None
                } else {
                    match phase {
                        Phase::Plan => Some(Phase::Plan),
                        // `auto` observed: no information — never overwrite our phase.
                        // (Dropping this used to kick a Plan session into AutoImplement,
                        // which would now fire on the first transcript line of *every*
                        // Plan session, since Plan itself runs CC in auto.)
                        _ => None,
                    }
                };
                s.last_activity = now();
                if let Some(phase) = reconciled {
                    s.phase = phase;
                    s.mode = phase.operator_mode();
                    let updated = s.clone();
                    self.persist(&updated);
                    self.audit(&session, AuditAction::PhaseChanged { to: phase }, false);
                    self.bus
                        .publish(EngineEvent::PhaseTransitioned { session, phase });
                }
            }
            DetectionEvent::TitleObserved { session, title } => {
                if let Some(s) = self.fleet.get_mut(&session) {
                    s.title = Some(title);
                    s.last_activity = now();
                    let upserted = s.clone();
                    // Persist so a rehydrated session keeps its name (UPDATE-only:
                    // a no-op for merely-observed sessions).
                    self.persist(&upserted);
                    self.bus
                        .publish(EngineEvent::SessionUpserted { session: upserted });
                } else {
                    tracing::warn!(session = %session, "TitleObserved for unknown session");
                }
            }
            DetectionEvent::WorkspaceObserved { session, path } => {
                if let Some(s) = self.fleet.get_mut(&session) {
                    if s.attached_path.as_deref() != Some(path.as_str()) {
                        s.attached_path = Some(path);
                        s.last_activity = now();
                        let upserted = s.clone();
                        self.bus
                            .publish(EngineEvent::SessionUpserted { session: upserted });
                    }
                } else {
                    tracing::warn!(session = %session, "WorkspaceObserved for unknown session");
                }
            }
            DetectionEvent::Discovered { session } => {
                if !self.fleet.contains_key(&session) {
                    let mut s = discovered_session(session.clone());
                    // App-managed sessions (created/imported via the cockpit) are
                    // recorded in the store — seed their operator state from it so an
                    // adopted record is gated the moment it's discovered, instead of
                    // needing a manual adopt. Sessions with no record stay observe-only.
                    if let Some(store) = &self.store {
                        if let Ok(Some(rec)) = store.managed(&session) {
                            s.adopted = rec.adopted;
                            s.phase = rec.phase;
                            s.mode = rec.mode;
                            // Restore the persisted trust tier too: it was seeded at launch
                            // from the project default and may carry a per-session operator
                            // override, so a discovered managed session is gated at its own
                            // tier immediately (not the default-deny `Observed`).
                            s.trust_tier = rec.trust_tier;
                        }
                    }
                    self.fleet.insert(session.clone(), s.clone());
                    tracing::info!(session = %session, adopted = s.adopted, "session discovered");
                    self.bus
                        .publish(EngineEvent::SessionUpserted { session: s });
                }
            }
            DetectionEvent::Ended { session } => {
                // Inactivity timeout, NOT completion — mark Idle (never Done, which
                // would fire a false ReviewReady). Keep adopted sessions so a >window
                // gap doesn't drop their governance; evict only unadopted ones.
                match self.fleet.get(&session) {
                    Some(s) if s.adopted => {
                        if let Some(s) = self.fleet.get_mut(&session) {
                            s.status = SessionStatus::Idle;
                            let upserted = s.clone();
                            tracing::debug!(session = %session, "adopted session idle (kept)");
                            self.bus
                                .publish(EngineEvent::SessionUpserted { session: upserted });
                        }
                    }
                    Some(_) => {
                        self.fleet.remove(&session);
                        tracing::info!(session = %session, "session ended (idle)");
                        self.bus.publish(EngineEvent::SessionStateChanged {
                            session,
                            status: SessionStatus::Idle,
                        });
                    }
                    None => {}
                }
            }
            DetectionEvent::PlanProposed { session, plan } => {
                // Surface the proposed plan to the operator (plan-review gate). A
                // pass-through fact for a tracked session.
                if self.fleet.contains_key(&session) {
                    tracing::info!(session = %session, "plan proposed");
                    self.bus
                        .publish(EngineEvent::PlanProposed { session, plan });
                }
            }
            DetectionEvent::SummaryObserved { session, summary } => {
                // Pass-through: the latest assistant prose for a tracked session,
                // surfaced as the review-gate "what this covers" summary (T4).
                if self.fleet.contains_key(&session) {
                    self.bus
                        .publish(EngineEvent::SummaryObserved { session, summary });
                }
            }
        }
    }

    async fn apply_status(&mut self, session: SessionId, status: SessionStatus) {
        if let Some(s) = self.fleet.get_mut(&session) {
            s.status = status;
            s.last_activity = now();
            let updated = s.clone();
            self.persist(&updated);
        } else {
            tracing::warn!(session = %session, "StatusChanged for unknown session");
            return;
        }

        self.bus.publish(EngineEvent::SessionStateChanged {
            session: session.clone(),
            status,
        });

        // Workflow advancement on a done-checkpoint (replaces the old FR13
        // revert-to-Plan). CC's own done-signal advances only the phase CC owns
        // (AutoImplement); the operator-confirmed gates (Plan via the plan-approval
        // keystone, Test/Review/Commit via their buttons) hold here. A pinned
        // session never auto-moves — it raises a request instead (A ≫ B).
        if status == SessionStatus::Done {
            let Some(s) = self.fleet.get(&session) else {
                return;
            };
            let phase = s.phase;
            if phase.auto_advances_on_done() {
                if s.phase_pinned {
                    self.bus.publish(EngineEvent::PhaseAdvanceRequested {
                        session,
                        to: phase.next(),
                    });
                } else {
                    self.apply_phase(session, phase.next(), false).await;
                }
            }
            // else: Plan/Test/Review/Commit await their operator gate; the session
            // stays `Done` so it surfaces as "ready" until the operator acts.
        }
    }

    /// Surface a control-port failure to the UI as an audit fact (architecture
    /// anti-pattern: never swallow a `Result` or panic).
    ///
    /// `Unavailable` is the **expected** degraded mode (NFR8): the active control
    /// adapter is observe-only, so steering/approval can't be delivered yet. That is
    /// not an error — it is logged quietly and surfaced as a plain note, not a
    /// red "control error". Any other failure stays loud.
    fn surface_control_error(
        &self,
        session: &SessionId,
        op: &str,
        err: moonlight_domain::errors::ControlError,
    ) {
        use moonlight_domain::errors::ControlError;
        let summary = if matches!(err, ControlError::Unavailable) {
            tracing::debug!(session = %session, op, "control is observe-only; {op} not delivered");
            format!("{op} not delivered — control is observe-only")
        } else {
            tracing::error!(session = %session, op, error = %err, "control port call failed");
            format!("control error during {op}: {err}")
        };
        self.bus.publish(EngineEvent::AuditAppended {
            session: session.clone(),
            summary,
        });
    }
}

/// Wall-clock now as epoch millis (domain has no clock; the supervisor stamps
/// observed activity).
fn now() -> Timestamp {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Timestamp::from_millis(ms)
}

/// Default session record for a session first observed via detection (it may have
/// been started outside MoonlightCode). Default-deny posture.
fn discovered_session(id: SessionId) -> Session {
    Session {
        id,
        title: None,
        status: SessionStatus::Idle,
        phase: Phase::Plan,
        mode: Mode::Auto,
        trust_tier: TrustTier::Observed,
        attached_path: None,
        pinned: false,
        adopted: false,
        paused: false,
        phase_pinned: false,
        hidden: false,
        last_activity: now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use moonlight_domain::errors::{ControlError, StoreError};
    use moonlight_domain::ports::control::ControlLevel;
    use moonlight_domain::ports::store::{ManagedSession, ManagedStateUpdate};
    use std::sync::Mutex;
    use tokio::sync::broadcast::error::TryRecvError;

    /// In-memory `ManagedSessionStore` recording the supervisor's persistence calls.
    /// `update_managed_state` reports the session as managed (`Ok(true)`) so the
    /// wiring is observable; the other methods are unused by the supervisor.
    #[derive(Default)]
    struct FakeStore {
        updates: Mutex<Vec<ManagedStateUpdate>>,
        audits: Mutex<Vec<AuditEntry>>,
        /// Optional managed record returned by `managed()` — lets a test simulate an
        /// app-created/imported session the supervisor should auto-adopt on discovery.
        managed_rec: Option<ManagedSession>,
        /// Records returned by `all_managed()` — lets a test simulate a populated
        /// store the supervisor rehydrates the fleet from at boot.
        managed_all: Vec<ManagedSession>,
    }

    impl FakeStore {
        fn updates(&self) -> Vec<ManagedStateUpdate> {
            self.updates.lock().unwrap().clone()
        }
        fn audits(&self) -> Vec<AuditEntry> {
            self.audits.lock().unwrap().clone()
        }
    }

    impl ManagedSessionStore for FakeStore {
        fn upsert_managed(&self, _s: &ManagedSession) -> Result<(), StoreError> {
            Ok(())
        }
        fn update_managed_state(&self, u: &ManagedStateUpdate) -> Result<bool, StoreError> {
            self.updates.lock().unwrap().push(u.clone());
            Ok(true)
        }
        fn set_conversation_id(
            &self,
            _id: &SessionId,
            _conversation_id: &str,
        ) -> Result<bool, StoreError> {
            Ok(true)
        }
        fn managed(&self, _id: &SessionId) -> Result<Option<ManagedSession>, StoreError> {
            Ok(self.managed_rec.clone())
        }
        fn all_managed(&self) -> Result<Vec<ManagedSession>, StoreError> {
            Ok(self.managed_all.clone())
        }
        fn remove_managed(&self, _id: &SessionId) -> Result<(), StoreError> {
            Ok(())
        }
        fn append_audit(&self, entry: &AuditEntry) -> Result<(), StoreError> {
            self.audits.lock().unwrap().push(entry.clone());
            Ok(())
        }
        fn recent_audit(
            &self,
            _session: &SessionId,
            _limit: usize,
        ) -> Result<Vec<AuditEntry>, StoreError> {
            Ok(Vec::new())
        }
    }

    /// Records every control-port call and hands back canned `Ok` results.
    #[derive(Default)]
    struct FakeControl {
        calls: Mutex<Vec<String>>,
    }

    impl FakeControl {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn record(&self, call: impl Into<String>) {
            self.calls.lock().unwrap().push(call.into());
        }
    }

    #[async_trait]
    impl ControlPort for FakeControl {
        fn level(&self) -> ControlLevel {
            ControlLevel::Govern
        }
        async fn spawn(
            &self,
            prompt: &str,
            attached_path: Option<&str>,
            starting_phase: Phase,
        ) -> Result<SessionId, ControlError> {
            self.record(format!(
                "spawn(prompt={prompt:?}, path={attached_path:?}, phase={starting_phase:?})"
            ));
            Ok(SessionId::new("sess-1"))
        }
        async fn inject_feedback(&self, feedback: &Feedback) -> Result<(), ControlError> {
            self.record(format!(
                "inject_feedback(session={}, origin={:?})",
                feedback.session_id, feedback.origin
            ));
            Ok(())
        }
        async fn set_phase(&self, session: &SessionId, phase: Phase) -> Result<(), ControlError> {
            self.record(format!("set_phase(session={session}, phase={phase:?})"));
            Ok(())
        }
        async fn pause(&self, session: &SessionId) -> Result<(), ControlError> {
            self.record(format!("pause(session={session})"));
            Ok(())
        }
        async fn resume(&self, session: &SessionId) -> Result<(), ControlError> {
            self.record(format!("resume(session={session})"));
            Ok(())
        }
    }

    /// Drain everything currently buffered on a receiver (non-blocking).
    fn drain(rx: &mut tokio::sync::broadcast::Receiver<EngineEvent>) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        out
    }

    fn fixture() -> (
        SessionSupervisor,
        Arc<FakeControl>,
        tokio::sync::broadcast::Receiver<EngineEvent>,
    ) {
        let control = Arc::new(FakeControl::default());
        let bus = EventBus::new(64);
        let rx = bus.subscribe();
        let sup = SessionSupervisor::new(control.clone(), bus);
        (sup, control, rx)
    }

    #[tokio::test]
    async fn discovered_session_auto_adopts_from_store_record() {
        let control = Arc::new(FakeControl::default());
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        // App-created/imported session recorded as adopted in the store.
        let store = Arc::new(FakeStore {
            managed_rec: Some(ManagedSession {
                id: SessionId::new("m1"),
                agent: moonlight_domain::AgentKind::ClaudeCode,
                conversation_id: None,
                // A per-session trust override the operator set earlier — discovery must
                // restore it, not reseed the default-deny `Observed`.
                trust_tier: TrustTier::Trusted,
                root: Some("/repo".into()),
                title: None,
                mode: Mode::Auto,
                phase: Phase::Review,
                adopted: true,
                paused: false,
                phase_pinned: false,
                hidden: false,
                created_at: now(),
                last_seen: now(),
            }),
            ..Default::default()
        });
        let mut sup = SessionSupervisor::with_store(control, bus, Some(store as Arc<_>));

        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("m1"),
        })
        .await;

        let s = sup.session(&SessionId::new("m1")).expect("discovered");
        assert!(
            s.adopted,
            "an app-managed record must auto-adopt on discovery"
        );
        assert_eq!(s.phase, Phase::Review, "phase seeded from the record");
        assert_eq!(s.mode, Mode::Auto, "mode seeded from the record");
        assert_eq!(
            s.trust_tier,
            TrustTier::Trusted,
            "per-session trust restored from the record on discovery"
        );
        assert!(
            matches!(&drain(&mut rx)[..], [EngineEvent::SessionUpserted { session }] if session.adopted)
        );
    }

    #[tokio::test]
    async fn discovered_session_without_record_stays_observe_only() {
        // Default store returns no record → unadopted (day-one safety for external sessions).
        let control = Arc::new(FakeControl::default());
        let bus = EventBus::new(64);
        let store = Arc::new(FakeStore::default());
        let mut sup = SessionSupervisor::with_store(control, bus, Some(store as Arc<_>));

        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("ext"),
        })
        .await;

        assert!(!sup.session(&SessionId::new("ext")).unwrap().adopted);
    }

    #[tokio::test]
    async fn hydrate_from_store_seeds_the_fleet_and_publishes_upserts() {
        let control = Arc::new(FakeControl::default());
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let store = Arc::new(FakeStore {
            managed_all: vec![
                ManagedSession {
                    id: SessionId::new("a"),
                    agent: moonlight_domain::AgentKind::ClaudeCode,
                    conversation_id: None,
                    trust_tier: TrustTier::Observed,
                    root: Some("/repo/a".into()),
                    title: None,
                    mode: Mode::Auto,
                    phase: Phase::AutoImplement,
                    adopted: true,
                    paused: false,
                    phase_pinned: true,
                    hidden: false,
                    created_at: now(),
                    last_seen: now(),
                },
                ManagedSession {
                    id: SessionId::new("b"),
                    agent: moonlight_domain::AgentKind::ClaudeCode,
                    conversation_id: None,
                    trust_tier: TrustTier::Observed,
                    root: Some("/repo/b".into()),
                    title: None,
                    mode: Mode::Auto,
                    phase: Phase::Plan,
                    adopted: false,
                    paused: false,
                    phase_pinned: false,
                    hidden: false,
                    created_at: now(),
                    last_seen: now(),
                },
            ],
            ..Default::default()
        });
        let mut sup = SessionSupervisor::with_store(control, bus, Some(store as Arc<_>));

        sup.hydrate_from_store();

        // Each persisted record lands in the live fleet, restored faithfully and at
        // rest (Idle — detection promotes it to Running only if it is actually live).
        let a = sup.session(&SessionId::new("a")).expect("a rehydrated");
        assert_eq!(a.phase, Phase::AutoImplement);
        assert_eq!(a.mode, Mode::Auto);
        assert!(a.adopted);
        assert!(a.phase_pinned);
        assert_eq!(a.attached_path.as_deref(), Some("/repo/a"));
        assert_eq!(a.status, SessionStatus::Idle);
        assert!(sup.session(&SessionId::new("b")).is_some());

        // ...and each is announced so the delta-fed cockpit grid can show it.
        let events = drain(&mut rx);
        assert_eq!(events.len(), 2, "one upsert per restored session");
        assert!(events
            .iter()
            .all(|e| matches!(e, EngineEvent::SessionUpserted { .. })));
    }

    #[tokio::test]
    async fn hydrate_from_store_does_not_clobber_a_live_session() {
        let control = Arc::new(FakeControl::default());
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let store = Arc::new(FakeStore {
            // The persisted record rests Idle in phase Plan.
            managed_all: vec![ManagedSession {
                id: SessionId::new("s"),
                agent: moonlight_domain::AgentKind::ClaudeCode,
                conversation_id: None,
                trust_tier: TrustTier::Observed,
                root: Some("/repo".into()),
                title: None,
                mode: Mode::Auto,
                phase: Phase::Plan,
                adopted: false,
                paused: false,
                hidden: false,
                phase_pinned: false,
                created_at: now(),
                last_seen: now(),
            }],
            ..Default::default()
        });
        let mut sup = SessionSupervisor::with_store(control, bus, Some(store as Arc<_>));
        // A live session already in the fleet, driven to Running by observed activity.
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        sup.on_detection(DetectionEvent::StatusChanged {
            session: SessionId::new("s"),
            status: SessionStatus::Running,
        })
        .await;
        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().status,
            SessionStatus::Running
        );
        let _ = drain(&mut rx);

        sup.hydrate_from_store();

        // The stale persisted (Idle) record must not overwrite the fresher live state,
        // and no upsert is published for the already-known session.
        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().status,
            SessionStatus::Running,
            "rehydration leaves a live session untouched"
        );
        assert!(
            drain(&mut rx).is_empty(),
            "no upsert for an already-live session"
        );
    }

    #[tokio::test]
    async fn forget_session_drops_the_fleet_entry_and_announces() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        let _ = drain(&mut rx);

        sup.handle_command(Command::ForgetSession {
            session: SessionId::new("s"),
        })
        .await;

        assert!(
            sup.session(&SessionId::new("s")).is_none(),
            "dropped from fleet"
        );
        assert!(matches!(
            &drain(&mut rx)[..],
            [EngineEvent::SessionRemoved { session }] if session == &SessionId::new("s")
        ));
    }

    #[tokio::test]
    async fn set_adopted_is_idempotent() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        let _ = drain(&mut rx);

        sup.handle_command(Command::SetAdopted {
            session: SessionId::new("s"),
            adopted: true,
        })
        .await;
        assert!(sup.session(&SessionId::new("s")).unwrap().adopted);
        assert_eq!(drain(&mut rx).len(), 1, "adopting publishes one upsert");

        // Setting the same value again changes nothing and publishes nothing.
        sup.handle_command(Command::SetAdopted {
            session: SessionId::new("s"),
            adopted: true,
        })
        .await;
        assert!(drain(&mut rx).is_empty(), "idempotent set is silent");
    }

    #[tokio::test]
    async fn set_hidden_masks_and_unmasks_republishing_the_row() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        let _ = drain(&mut rx);

        // Hide it → the row carries hidden=true and is republished so the grid drops it.
        sup.handle_command(Command::SetHidden {
            session: SessionId::new("s"),
            hidden: true,
        })
        .await;
        assert!(sup.session(&SessionId::new("s")).unwrap().hidden);
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            EngineEvent::SessionUpserted { session } if session.hidden
        ));

        // Unhide restores it.
        sup.handle_command(Command::SetHidden {
            session: SessionId::new("s"),
            hidden: false,
        })
        .await;
        assert!(!sup.session(&SessionId::new("s")).unwrap().hidden);
    }

    #[tokio::test]
    async fn set_trust_updates_tier_and_republishes() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        let _ = drain(&mut rx);

        sup.handle_command(Command::SetTrust {
            session: SessionId::new("s"),
            tier: TrustTier::Trusted,
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().trust_tier,
            TrustTier::Trusted
        );
        // Republished so the gate's PDP picks up the new tier; idempotent thereafter.
        assert!(matches!(
            &drain(&mut rx)[..],
            [EngineEvent::SessionUpserted { session }] if session.trust_tier == TrustTier::Trusted
        ));
        sup.handle_command(Command::SetTrust {
            session: SessionId::new("s"),
            tier: TrustTier::Trusted,
        })
        .await;
        assert!(drain(&mut rx).is_empty(), "idempotent set is silent");
    }

    #[tokio::test]
    async fn set_phase_pins_and_reaches_engine_only_phases() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        let _ = drain(&mut rx);

        // The operator can jump straight to an engine-only phase (Test/Review/Commit)
        // that no automatic transition reaches — and the manual pick **pins** it.
        sup.handle_command(Command::SetPhase {
            session: SessionId::new("s"),
            phase: Phase::Test,
        })
        .await;

        let s = sup.session(&SessionId::new("s")).unwrap();
        assert_eq!(s.phase, Phase::Test);
        assert_eq!(s.mode, Mode::Auto, "Test surfaces as the Auto substrate");
        assert!(s.phase_pinned, "a manual pick pins the phase (A ≫ B)");
        assert!(
            drain(&mut rx).iter().any(|e| matches!(e,
                EngineEvent::SessionUpserted { session }
                    if session.phase == Phase::Test && session.phase_pinned)),
            "SetPhase republishes the full row carrying the pinned Test phase"
        );
    }

    #[tokio::test]
    async fn autoimplement_auto_advances_to_test_on_done() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        // Move to AutoImplement by advancing the workflow (unpinned, the
        // auto-advancing phase). Detection can't put us here any more: every phase
        // runs CC in `auto`, so an observed mode never implies a phase.
        sup.handle_command(Command::AdvancePhase {
            session: SessionId::new("s"),
        })
        .await;
        let _ = drain(&mut rx);

        sup.on_detection(DetectionEvent::StatusChanged {
            session: SessionId::new("s"),
            status: SessionStatus::Done,
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::Test,
            "CC's done-signal auto-advances AutoImplement → Test"
        );
    }

    #[tokio::test]
    async fn plan_does_not_auto_advance_on_done() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        sup.on_detection(DetectionEvent::PhaseObserved {
            session: SessionId::new("s"),
            phase: Phase::Plan,
        })
        .await;
        let _ = drain(&mut rx);

        sup.on_detection(DetectionEvent::StatusChanged {
            session: SessionId::new("s"),
            status: SessionStatus::Done,
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::Plan,
            "Plan waits for the plan-approval keystone, not CC's done-signal"
        );
    }

    #[tokio::test]
    async fn observing_cc_auto_never_moves_a_plan_session() {
        // Plan runs CC in `auto` like every other phase, so an observed `auto` carries
        // no information. Acting on it would kick every Plan session into AutoImplement
        // on its first transcript line — and silently unfreeze project writes.
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        sup.on_detection(DetectionEvent::PhaseObserved {
            session: SessionId::new("s"),
            phase: Phase::Plan,
        })
        .await;
        let _ = drain(&mut rx);

        sup.on_detection(DetectionEvent::PhaseObserved {
            session: SessionId::new("s"),
            phase: Phase::AutoImplement,
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::Plan,
            "observed `auto` must not move a session off Plan"
        );
    }

    #[tokio::test]
    async fn approving_a_plan_hands_the_session_off_to_auto() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        sup.on_detection(DetectionEvent::PhaseObserved {
            session: SessionId::new("s"),
            phase: Phase::Plan,
        })
        .await;
        let _ = drain(&mut rx);

        sup.handle_command(Command::ApprovePlan {
            session: SessionId::new("s"),
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::AutoImplement,
            "approving the plan is the keystone that moves Plan → Auto"
        );
    }

    #[tokio::test]
    async fn approving_a_plan_on_a_pinned_session_only_requests_the_advance() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        // A manual pick pins Plan (A ≫ B).
        sup.handle_command(Command::SetPhase {
            session: SessionId::new("s"),
            phase: Phase::Plan,
        })
        .await;
        let _ = drain(&mut rx);

        sup.handle_command(Command::ApprovePlan {
            session: SessionId::new("s"),
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::Plan,
            "a pinned phase is never moved silently"
        );
        assert!(
            drain(&mut rx).iter().any(|e| matches!(e,
                EngineEvent::PhaseAdvanceRequested { to, .. } if *to == Phase::AutoImplement)),
            "pinned + plan approved raises a PhaseAdvanceRequested(Auto) instead"
        );
    }

    #[tokio::test]
    async fn approving_a_plan_outside_plan_leaves_the_phase_alone() {
        // A native plan proposed from, say, Review: the phase is the operator's
        // business — never jump the workflow backwards into implementation.
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        sup.handle_command(Command::SetPhase {
            session: SessionId::new("s"),
            phase: Phase::Review,
        })
        .await;
        let _ = drain(&mut rx);

        sup.handle_command(Command::ApprovePlan {
            session: SessionId::new("s"),
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::Review
        );
    }

    #[tokio::test]
    async fn pinned_session_requests_advance_instead_of_moving_on_done() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        // Pin AutoImplement (a manual pick pins).
        sup.handle_command(Command::SetPhase {
            session: SessionId::new("s"),
            phase: Phase::AutoImplement,
        })
        .await;
        let _ = drain(&mut rx);

        sup.on_detection(DetectionEvent::StatusChanged {
            session: SessionId::new("s"),
            status: SessionStatus::Done,
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::AutoImplement,
            "a pinned session does not auto-move"
        );
        assert!(
            drain(&mut rx).iter().any(|e| matches!(e,
                EngineEvent::PhaseAdvanceRequested { to, .. } if *to == Phase::Test)),
            "pinned + done raises a PhaseAdvanceRequested(Test) instead"
        );
    }

    #[tokio::test]
    async fn advance_phase_moves_to_next_unpins_and_announces_review() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        // Pin Test (manual pick), then operator-confirm the advance.
        sup.handle_command(Command::SetPhase {
            session: SessionId::new("s"),
            phase: Phase::Test,
        })
        .await;
        let _ = drain(&mut rx);

        sup.handle_command(Command::AdvancePhase {
            session: SessionId::new("s"),
        })
        .await;

        let s = sup.session(&SessionId::new("s")).unwrap();
        assert_eq!(s.phase, Phase::Review, "Test → Review on advance");
        assert!(
            !s.phase_pinned,
            "advancing returns the session to auto mode"
        );
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, EngineEvent::ReviewReady { .. })),
            "entering Review opens the code-review gate"
        );
    }

    #[tokio::test]
    async fn detection_does_not_clobber_a_pinned_phase() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        // Pin Test (auto substrate). A subsequent observed `plan` must NOT win.
        sup.handle_command(Command::SetPhase {
            session: SessionId::new("s"),
            phase: Phase::Test,
        })
        .await;
        let _ = drain(&mut rx);

        sup.on_detection(DetectionEvent::PhaseObserved {
            session: SessionId::new("s"),
            phase: Phase::Plan,
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::Test,
            "a pinned phase survives a contradicting detection observation"
        );
    }

    #[tokio::test]
    async fn commit_advance_loops_back_to_plan() {
        let (mut sup, _control, mut rx) = fixture();
        sup.on_detection(DetectionEvent::Discovered {
            session: SessionId::new("s"),
        })
        .await;
        sup.handle_command(Command::SetPhase {
            session: SessionId::new("s"),
            phase: Phase::Commit,
        })
        .await;
        let _ = drain(&mut rx);

        sup.handle_command(Command::AdvancePhase {
            session: SessionId::new("s"),
        })
        .await;

        assert_eq!(
            sup.session(&SessionId::new("s")).unwrap().phase,
            Phase::Plan,
            "Commit → Plan starts the next cycle"
        );
    }

    #[tokio::test]
    async fn spawn_calls_port_and_publishes_state_changed() {
        let (mut sup, control, mut rx) = fixture();

        sup.handle_command(Command::SpawnSession {
            prompt: "build the thing".into(),
            attached_path: Some("/repo".into()),
        })
        .await;

        // Right port call, with the default Plan starting phase.
        let calls = control.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("spawn("), "got {calls:?}");
        assert!(calls[0].contains("phase=Plan"), "got {calls:?}");

        // Session is now tracked.
        let id = SessionId::new("sess-1");
        let s = sup.session(&id).expect("session inserted");
        assert_eq!(s.status, SessionStatus::Running);
        assert_eq!(s.phase, Phase::Plan);
        assert_eq!(s.mode, Mode::Auto, "every phase runs CC in auto");

        // And the right fact landed on the bus: a full-row upsert (so the UI can
        // build a tile), carrying the spawned defaults.
        let events = drain(&mut rx);
        assert!(
            matches!(&events[..], [EngineEvent::SessionUpserted { session }]
                if session.id == id
                    && session.status == SessionStatus::Running
                    && session.phase == Phase::Plan
                    && session.mode == Mode::Auto),
            "got {events:?}"
        );
    }

    #[tokio::test]
    async fn discovered_publishes_full_row_upsert() {
        let (mut sup, _control, mut rx) = fixture();
        let id = SessionId::new("ext-7");

        sup.on_detection(DetectionEvent::Discovered {
            session: id.clone(),
        })
        .await;

        assert!(sup.session(&id).is_some(), "discovered session tracked");
        let events = drain(&mut rx);
        assert!(
            matches!(&events[..], [EngineEvent::SessionUpserted { session }]
                if session.id == id && session.status == SessionStatus::Idle),
            "got {events:?}"
        );
    }

    #[tokio::test]
    async fn title_observed_publishes_upsert_with_title() {
        let (mut sup, _control, mut rx) = fixture();
        let id = SessionId::new("ext-7");

        sup.on_detection(DetectionEvent::Discovered {
            session: id.clone(),
        })
        .await;
        let _ = drain(&mut rx);

        sup.on_detection(DetectionEvent::TitleObserved {
            session: id.clone(),
            title: "Refactor auth".into(),
        })
        .await;

        let events = drain(&mut rx);
        assert!(
            matches!(&events[..], [EngineEvent::SessionUpserted { session }]
                if session.id == id && session.title.as_deref() == Some("Refactor auth")),
            "got {events:?}"
        );
    }

    #[tokio::test]
    async fn workspace_observed_sets_path_and_upserts() {
        let (mut sup, _control, mut rx) = fixture();
        let id = SessionId::new("ext-7");

        sup.on_detection(DetectionEvent::Discovered {
            session: id.clone(),
        })
        .await;
        let _ = drain(&mut rx);

        sup.on_detection(DetectionEvent::WorkspaceObserved {
            session: id.clone(),
            path: "/work/api".into(),
        })
        .await;

        assert_eq!(
            sup.session(&id).unwrap().attached_path.as_deref(),
            Some("/work/api")
        );
        let events = drain(&mut rx);
        assert!(
            matches!(&events[..], [EngineEvent::SessionUpserted { session }]
                if session.attached_path.as_deref() == Some("/work/api")),
            "got {events:?}"
        );
    }

    #[tokio::test]
    async fn steer_injects_feedback_and_audits() {
        let (mut sup, control, mut rx) = fixture();
        let id = SessionId::new("sess-1");

        sup.handle_command(Command::Steer {
            session: id.clone(),
            message: "cap retries at 3".into(),
        })
        .await;

        let calls = control.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("inject_feedback"), "got {calls:?}");
        assert!(calls[0].contains("OperatorSteer"), "got {calls:?}");

        let events = drain(&mut rx);
        assert!(
            matches!(&events[..], [EngineEvent::AuditAppended { session, summary }]
                if *session == id && summary.contains("cap retries at 3")),
            "got {events:?}"
        );
    }

    #[tokio::test]
    async fn reject_hunk_injects_via_the_port_and_audits() {
        // Option C: per-hunk feedback is delivered through the `ControlPort`
        // (the SteerControl adapter queues it for the UI), then audited.
        let (mut sup, control, mut rx) = fixture();
        let id = SessionId::new("s1");

        sup.handle_command(Command::RejectHunk {
            feedback: Feedback {
                session_id: id.clone(),
                message: "add exponential backoff, cap 3 retries".into(),
                origin: FeedbackOrigin::HunkRejection,
            },
        })
        .await;

        // Delivered through the port, tagged as a hunk rejection…
        assert!(
            control
                .calls()
                .iter()
                .any(|c| c.contains("inject_feedback") && c.contains("HunkRejection")),
            "expected inject_feedback(HunkRejection), got {:?}",
            control.calls()
        );
        // …and announced on the bus for the audit feed.
        let events = drain(&mut rx);
        assert!(
            events.iter().any(|e| matches!(e,
                EngineEvent::AuditAppended { summary, .. } if summary.contains("backoff"))),
            "expected an AuditAppended fact, got {events:?}"
        );
    }

    #[tokio::test]
    async fn approve_resumes_and_publishes_running() {
        let (mut sup, control, mut rx) = fixture();
        let id = SessionId::new("sess-1");

        sup.handle_command(Command::ApproveAction {
            session: id.clone(),
        })
        .await;

        assert_eq!(control.calls(), vec![format!("resume(session={id})")]);
        let events = drain(&mut rx);
        assert!(
            matches!(&events[..], [EngineEvent::SessionStateChanged { session, status }]
                if *session == id && *status == SessionStatus::Running),
            "got {events:?}"
        );
    }

    #[tokio::test]
    async fn done_in_autoimplement_advances_forward_not_back_to_plan() {
        let (mut sup, control, mut rx) = fixture();
        let id = SessionId::new("sess-1");

        // Spawn (lands in Plan, unpinned), then drive it into AutoImplement.
        sup.handle_command(Command::SpawnSession {
            prompt: "x".into(),
            attached_path: None,
        })
        .await;
        sup.handle_command(Command::AdvancePhase {
            session: id.clone(),
        })
        .await;
        assert_eq!(sup.session(&id).unwrap().phase, Phase::AutoImplement);

        // Drain prior events so we assert only on the "done" transition.
        let _ = drain(&mut rx);

        // The session declares it is done.
        sup.on_detection(DetectionEvent::StatusChanged {
            session: id.clone(),
            status: SessionStatus::Done,
        })
        .await;

        // New behavior (Part B): forward-progress to Test, **not** revert to Plan.
        assert_eq!(sup.session(&id).unwrap().phase, Phase::Test);
        assert!(
            !control
                .calls()
                .iter()
                .any(|c| c.contains("set_phase") && c.contains("phase=Plan")),
            "must NOT revert to Plan on done, got {:?}",
            control.calls()
        );

        // …and the facts: status Done, then a full-row upsert carrying Test.
        let events = drain(&mut rx);
        assert!(
            events.iter().any(|e| matches!(
                e,
                EngineEvent::SessionStateChanged {
                    status: SessionStatus::Done,
                    ..
                }
            )),
            "missing Done state change in {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                EngineEvent::SessionUpserted { session } if session.phase == Phase::Test
            )),
            "missing SessionUpserted(Test) in {events:?}"
        );
    }

    #[tokio::test]
    async fn entering_plan_injects_the_aim_and_publishes() {
        let (mut sup, control, mut rx) = fixture();
        let id = SessionId::new("sess-1");

        // Spawn then move to the AutoImplement phase.
        sup.handle_command(Command::SpawnSession {
            prompt: "x".into(),
            attached_path: None,
        })
        .await;
        sup.handle_command(Command::SetPhase {
            session: id.clone(),
            phase: Phase::AutoImplement,
        })
        .await;
        sup.on_detection(DetectionEvent::PhaseObserved {
            session: id.clone(),
            phase: Phase::AutoImplement,
        })
        .await;
        let _ = drain(&mut rx);

        // Switching back to Plan tells the agent the aim — CC is in `auto` either
        // way, so this injection is the only thing that makes it plan.
        sup.handle_command(Command::SetPhase {
            session: id.clone(),
            phase: Phase::Plan,
        })
        .await;

        assert_eq!(sup.session(&id).unwrap().mode, Mode::Auto);
        assert_eq!(sup.session(&id).unwrap().phase, Phase::Plan);
        assert!(
            control
                .calls()
                .iter()
                .any(|c| c.contains("inject_feedback") && c.contains("origin=PhaseAim")),
            "got {:?}",
            control.calls()
        );
        let events = drain(&mut rx);
        assert!(
            events.iter().any(|e| matches!(
                e,
                EngineEvent::SessionUpserted { session } if session.phase == Phase::Plan
            )),
            "got {events:?}"
        );
    }

    #[tokio::test]
    async fn persists_managed_state_and_audits_on_phase_change() {
        let control = Arc::new(FakeControl::default());
        let store = Arc::new(FakeStore::default());
        let bus = EventBus::new(64);
        let mut sup = SessionSupervisor::with_store(control, bus, Some(store.clone() as Arc<_>));
        let id = SessionId::new("sess-1");

        // Spawn (Plan), then move the phase → should persist + audit.
        sup.handle_command(Command::SpawnSession {
            prompt: "x".into(),
            attached_path: None,
        })
        .await;
        sup.handle_command(Command::AdvancePhase {
            session: id.clone(),
        })
        .await;

        // The managed row was refreshed (UPDATE-only) with the new phase/mode.
        let updates = store.updates();
        assert!(
            updates
                .iter()
                .any(|u| u.id == id && u.phase == Phase::AutoImplement && u.mode == Mode::Auto),
            "expected a managed-state update, got {updates:?}"
        );

        // …and a PhaseChanged audit entry was appended (with a time-ordered id).
        let audits = store.audits();
        assert!(
            audits.iter().any(|a| a.session_id == id
                && matches!(
                    a.action,
                    AuditAction::PhaseChanged {
                        to: Phase::AutoImplement
                    }
                )),
            "expected a PhaseChanged audit, got {audits:?}"
        );
        assert!(audits.iter().all(|a| !a.id.is_empty()));
    }

    #[tokio::test]
    async fn no_store_means_no_persistence_calls_and_no_panic() {
        // The pure constructor must keep working: a phase change just publishes.
        let (mut sup, _control, mut rx) = fixture();
        sup.handle_command(Command::SpawnSession {
            prompt: "x".into(),
            attached_path: None,
        })
        .await;
        sup.handle_command(Command::AdvancePhase {
            session: SessionId::new("sess-1"),
        })
        .await;
        let events = drain(&mut rx);
        assert!(events.iter().any(|e| matches!(
            e,
            EngineEvent::SessionUpserted { session } if session.phase == Phase::AutoImplement
        )));
    }
}
