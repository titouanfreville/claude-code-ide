//! A small, app-wide HTTP control surface for external clients that sit outside
//! any single Claude Code session — an editor extension, not an agent. Distinct
//! from [`crate::serve_http`]/[`crate::McpHost`], which bind one ephemeral, private
//! endpoint **per session** for that session's own MCP tool calls: this is one
//! stable endpoint for the whole running app, covering state that spans sessions
//! (the review queue) or belongs to none of them (gating status).
//!
//! Kept deliberately narrow for its first cut: the review queue (list + accept +
//! reject) and the hook-gating status the operator needs visible at a glance. Both
//! reuse the exact same [`moonlight_engine::Command`] channel and domain store
//! ports the desktop UI already drives — no new state, no new policy.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

pub use moonlight_control::HookStatusEntry;
use moonlight_domain::changes::{
    Baseline, ChangeTool, CommentAuthor, CommentScope, DiffSide, ReviewComment,
};
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::{ManagedSessionStore, SessionChangeStore};
use moonlight_domain::review::FeedbackOrigin;
use moonlight_domain::session::SessionStatus;
use moonlight_domain::{Feedback, SessionId, Timestamp};
use moonlight_engine::Command;
use moonlight_engine::EventBus;

use crate::adapters::BusFleetView;

/// One file a managed, adopted session has written and not yet had reviewed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewQueueItem {
    pub session_id: String,
    pub session_title: Option<String>,
    pub file_path: String,
    /// How many times the session wrote this file.
    pub touches: u32,
    /// The tool that wrote it most recently.
    pub tool: ChangeTool,
    /// The session created the file — the "before" side is empty.
    pub created: bool,
    /// The baseline came from VCS rather than an observed pre-image, so the "before"
    /// side may include edits that were already uncommitted. Surfaced because a
    /// reviewer reading an inferred baseline as an exact one draws wrong conclusions.
    pub from_head: bool,
    /// Unified-diff text against the session's baseline; empty when either side
    /// couldn't be read (the item still appears so the operator knows something
    /// changed, rather than silently dropping it from the queue).
    pub diff: String,
}

#[derive(Debug, Deserialize)]
pub struct AcceptRequest {
    pub session_id: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct RejectRequest {
    pub session_id: String,
    pub path: String,
    /// One-line steering, delivered to the session as rejection-as-feedback —
    /// the same primitive the desktop review surface uses.
    pub message: String,
}

/// Whether a rejection's feedback can actually reach the agent.
///
/// Rejecting does two separable things: it drops the file from the queue (always),
/// and it steers the session with the operator's message (only if something can
/// carry it). A headless backend has no PTY to write into, so the second half is a
/// no-op — and reporting a bare success for that is the same fail-open-looks-like-
/// success mistake this control surface exists to avoid.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FeedbackDelivery {
    /// Handed to the control port, which writes it into the session's terminal.
    Queued,
    /// Nothing is listening for steering, so the message was dropped. The file is
    /// still marked reviewed — only the message is lost.
    Undeliverable { reason: String },
}

/// The result of a reject: what happened to the file, and what happened to the
/// message. Separate fields because they genuinely can differ.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RejectResponse {
    /// The file left the review queue (so it stops nagging on every poll).
    pub reviewed: bool,
    pub feedback: FeedbackDelivery,
}

/// A session detection has found — adopted or not. An unadopted one exists only in
/// the live fleet (see [`BusFleetView`]), never the durable store, so this is the
/// only way an external client (an editor extension) can see it at all in order to
/// offer "adopt this session."
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiscoverableSession {
    pub session_id: String,
    pub title: Option<String>,
    pub root: Option<String>,
    pub adopted: bool,
    /// Live status from detection — **not** from Claude Code, which exposes no
    /// idle/mid-turn signal. A client uses this to refuse opening a review while the
    /// session is `Running`: reviewing a file the agent is still writing produces
    /// comments about code that no longer exists by the time they are read.
    pub status: SessionStatus,
    /// Workflow phase. Meaningful only while `adopted` — an unadopted session is
    /// never gated, so its phase governs nothing. Exposed because an adopted
    /// session sitting in [`Phase::Plan`] has its project writes denied, and a
    /// client that can't see that can't explain why the agent is stuck.
    pub phase: Phase,
    /// How many files this session has written that nobody has reviewed yet — the
    /// size of *its* slice of the review queue.
    ///
    /// Carried on the session rather than counted from `/control/review-queue`
    /// because that endpoint renders a unified diff per file: polling it for a
    /// number would render every diff in the fleet every few seconds. This comes
    /// from one indexed read of the touch ledger.
    ///
    /// Always 0 for an unadopted session — the ledger only records adopted ones.
    pub unreviewed_files: usize,
}

#[derive(Debug, Deserialize)]
pub struct AdoptRequest {
    pub session_id: String,
}

/// The "before" side of a file, plus what kind of baseline it is — the plugin needs
/// the *content* (to render a real diff editor), not a pre-rendered unified diff.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaselineView {
    /// Exact pre-image, read before the writing tool was unblocked.
    Content { text: String },
    /// Inferred from VCS — may include changes that were already uncommitted.
    FromHead { text: String },
    /// The session created the file; the before side is empty.
    Created,
    /// No pre-image available; say so rather than showing a misleading empty pane.
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewCommentView {
    pub id: String,
    pub scope: CommentScope,
    pub path: String,
    pub side: DiffSide,
    pub start_line: u32,
    pub end_line: u32,
    pub body: String,
    pub anchor_text: Option<String>,
    /// Set once the batched review carrying this comment reached the session.
    pub sent: bool,
    pub resolved: bool,
    /// The anchored line no longer reads as it did when the comment was written, so
    /// the comment is probably about code that has since moved or changed.
    ///
    /// This is the expiry condition for a queued review — **not** a wall-clock TTL.
    /// A review that waited an hour for an idle session is still perfectly valid; one
    /// whose lines were rewritten thirty seconds later is not. Time can't tell those
    /// apart; the anchor can.
    pub outdated: bool,
    /// Who wrote it — the reviewer, or the session answering them. A client that
    /// cannot tell the two apart shows a review talking to itself.
    pub author: CommentAuthor,
    /// The comment this one answers; `None` for a thread root. Clients group on it to
    /// render one thread instead of a pile of unrelated notes at the same line.
    pub parent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddCommentRequest {
    pub session_id: String,
    pub scope: CommentScope,
    #[serde(default)]
    pub path: String,
    pub side: DiffSide,
    #[serde(default)]
    pub start_line: u32,
    #[serde(default)]
    pub end_line: u32,
    pub body: String,
    /// The line as it read when the comment was written — what makes "outdated"
    /// detectable later. Omitted for file- and review-scoped comments.
    #[serde(default)]
    pub anchor_text: Option<String>,
    /// Answer an existing comment instead of starting a new thread.
    ///
    /// The reply inherits that comment's scope, file, side and line range — the
    /// anchor fields above are ignored — because a reply that points somewhere else
    /// is not a reply. Replying to a reply attaches to the same thread root rather
    /// than nesting.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Who is writing. Defaults to the operator: the editor surfaces are the
    /// reviewer's, and a client that says nothing is one of those. A session
    /// answering its own review passes `Agent`, which is what keeps its words out of
    /// the next batch delivered *to* it.
    #[serde(default)]
    pub author: Option<CommentAuthor>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCommentRequest {
    pub session_id: String,
    pub id: String,
    /// New body, or `None` to leave it alone.
    #[serde(default)]
    pub body: Option<String>,
    /// Resolve / unresolve, or `None` to leave it alone.
    #[serde(default)]
    pub resolved: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteCommentRequest {
    pub session_id: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct SubmitReviewRequest {
    pub session_id: String,
}

/// What a submitted review did: how many comments went, and whether the message
/// actually reached the agent (see [`FeedbackDelivery`] — a headless backend has
/// nowhere to deliver it).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SubmitReviewResponse {
    /// How many comments the batch carried. They were actually **delivered** only if
    /// `feedback` is `queued` — naming this `sent` would repeat the mistake this
    /// endpoint exists to avoid.
    pub comment_count: usize,
    /// The comments this batch carried. A client that delivers the review itself
    /// (an editor writing into the session's terminal) confirms these back via
    /// `/control/review/delivered` — the server cannot see that delivery, and must
    /// not assume it happened.
    pub comment_ids: Vec<String>,
    /// Where the full review was written, if a root was known. The review is a file
    /// on disk rather than a message body: it has no length limit, the agent can
    /// re-read it, and it survives a delivery that is delayed or never happens.
    pub review_path: Option<String>,
    pub feedback: FeedbackDelivery,
}

/// A delivery the *client* performed and can vouch for — it wrote the review into
/// the session's terminal and pressed return. Only a client that actually did that
/// may call this: stamping `sent_at` is the record that the agent was told.
#[derive(Debug, Deserialize)]
pub struct MarkDeliveredRequest {
    pub session_id: String,
    pub comment_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetIgnoredRequest {
    pub session_id: String,
    pub path: String,
    pub ignored: bool,
}

#[derive(Debug, Deserialize)]
pub struct SetPhaseRequest {
    pub session_id: String,
    pub phase: Phase,
}

#[derive(Debug, Deserialize)]
pub struct AdvancePhaseRequest {
    pub session_id: String,
}

/// How long a fetched account quota is served before another network round-trip.
/// The figures move on the order of minutes and the fetch shells out to `curl`, so
/// a client polling every second must not turn into a request per second.
const QUOTA_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Account-wide Claude usage — the same figures the desktop status bar draws, read
/// from Anthropic's OAuth usage endpoint (see [`moonlight_core::obs::quota`]).
///
/// Every field is optional because every source is: a client that cannot see a
/// figure must render "—" rather than invent a zero, which would read as "plenty of
/// quota left" precisely when the truth is unknown.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct AccountUsage {
    /// Percentage of the rolling 5-hour window consumed (0–100).
    pub five_hour_pct: Option<u8>,
    /// When that window resets, as Unix epoch **seconds** (UTC), so a client can run
    /// its own live countdown instead of re-polling for a decreasing string.
    pub five_hour_resets_at: Option<i64>,
    /// The same reset pre-rendered as a compact countdown (`2h14m`, `47m`, `<1m`) at
    /// response time — for a client that just wants to print it.
    pub five_hour_resets_in: Option<String>,
    /// Percentage of the weekly (7-day) window consumed.
    pub weekly_pct: Option<u8>,
    /// Percentage of the weekly Sonnet-specific window consumed.
    pub sonnet_pct: Option<u8>,
}

/// Per-session observability: how long the session has been going and how full its
/// context window is. Sourced from the statusline snapshots Claude Code itself
/// writes, so it only exists for sessions MoonlightCode registered its statusline
/// with — a session without a snapshot is simply absent from the list.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionUsage {
    pub session_id: String,
    /// Title from the managed store, when this session is known there.
    pub title: Option<String>,
    /// Model display name, e.g. `Opus 5`.
    pub model: String,
    /// Output style / persona name.
    pub persona: String,
    /// Wall-clock session duration in ms, and the same figure pre-rendered (`1h12m`).
    pub session_ms: u64,
    pub session_uptime: String,
    /// Context-window occupancy as a percentage, when known. Claude Code's own
    /// figure when it reports one (it accounts for its own reserves), else tokens ÷
    /// window.
    pub ctx_pct: Option<u8>,
    pub ctx_tokens: u64,
    pub ctx_limit: u64,
    pub cost_usd: f64,
    /// When the snapshot was written (epoch ms) — a client can grey out a stale row
    /// rather than presenting a dead session's last reading as current.
    pub updated_ms: i64,
}

/// The `/control/usage` payload: one account-wide quota plus a row per observed
/// session.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsageResponse {
    /// `None` when no quota source could be read at all (no Claude credentials, or
    /// offline) — distinct from a quota whose individual windows are unknown.
    pub quota: Option<AccountUsage>,
    pub sessions: Vec<SessionUsage>,
}

/// Last fetched quota + when, so [`QUOTA_TTL`] can be honoured across requests.
type QuotaCache =
    std::sync::Mutex<Option<(std::time::Instant, Option<moonlight_core::obs::Quota>)>>;

/// Shared state behind the control API's handlers.
#[derive(Clone)]
pub struct ControlApiState {
    commands: UnboundedSender<Command>,
    sessions: Arc<dyn ManagedSessionStore>,
    changes: Arc<dyn SessionChangeStore>,
    hooks_status: Arc<dyn Fn() -> Vec<HookStatusEntry> + Send + Sync>,
    /// Monotonic tie-breaker for comment ids written in the same millisecond, so
    /// two comments added in quick succession still sort in the order they were
    /// written rather than colliding on one id.
    comment_seq: Arc<std::sync::atomic::AtomicU64>,
    /// Reports whether steering can currently reach a session. A closure rather
    /// than a flag so it reflects live runtime state (the desktop app's terminal
    /// can go away), and so the composition root — the only layer that knows what
    /// is listening — owns the answer.
    feedback_delivery: Arc<dyn Fn() -> FeedbackDelivery + Send + Sync>,
    fleet: Arc<BusFleetView>,
    /// Account-quota cache for `/control/usage` — see [`QUOTA_TTL`].
    quota: Arc<QuotaCache>,
    /// The engine bus, streamed to clients over `/control/events`.
    events: EventBus,
    /// Holds waiting on an operator. A client's verdict resolves one of these rather
    /// than reaching the supervisor.
    pending: Arc<moonlight_control::PendingApprovals>,
    /// Serves the workflow verbs to sessions, when this host offers them.
    ///
    /// Behind `mcp-transport` because the host lives in the rmcp-backed transport; a
    /// build without it simply has no MCP to hand out.
    #[cfg(feature = "mcp-transport")]
    mcp: Option<Arc<crate::McpHost>>,
}

impl ControlApiState {
    pub fn new(
        commands: UnboundedSender<Command>,
        sessions: Arc<dyn ManagedSessionStore>,
        changes: Arc<dyn SessionChangeStore>,
        hooks_status: impl Fn() -> Vec<HookStatusEntry> + Send + Sync + 'static,
        feedback_delivery: impl Fn() -> FeedbackDelivery + Send + Sync + 'static,
        fleet: Arc<BusFleetView>,
        events: EventBus,
    ) -> Self {
        Self {
            commands,
            sessions,
            changes,
            hooks_status: Arc::new(hooks_status),
            comment_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            feedback_delivery: Arc::new(feedback_delivery),
            fleet,
            events,
            pending: Arc::new(moonlight_control::PendingApprovals::new()),
            quota: Arc::new(std::sync::Mutex::new(None)),
            // Opt in with `with_mcp_host`: a host that binds no MCP must not claim to.
            #[cfg(feature = "mcp-transport")]
            mcp: None,
        }
    }
}

impl ControlApiState {
    fn next_comment_seq(&self) -> u64 {
        self.comment_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

/// The control API's routes, bound to `state`.
impl ControlApiState {
    /// Share the hold registry the gate uses, so a client's verdict resolves the very
    /// hook the gate is holding rather than a second, unrelated one.
    pub fn with_pending(mut self, pending: Arc<moonlight_control::PendingApprovals>) -> Self {
        self.pending = pending;
        self
    }
}

#[cfg(feature = "mcp-transport")]
impl ControlApiState {
    /// Serve MCP endpoints for sessions from this host.
    pub fn with_mcp_host(mut self, host: Arc<crate::McpHost>) -> Self {
        self.mcp = Some(host);
        self
    }
}

/// A session asking where its MCP endpoint is.
#[derive(Debug, serde::Deserialize)]
pub struct McpEndpointRequest {
    pub session_id: String,
}

#[derive(Debug, serde::Serialize)]
pub struct McpEndpointResponse {
    /// The URL to hand Claude Code as `--mcp-config`.
    pub url: String,
}

/// One verb, run on behalf of a session that reached us from another process.
#[derive(Debug, serde::Deserialize)]
pub struct VerbRequest {
    pub session_id: String,
    pub verb: moonlight_domain::trust::McpVerb,
    #[serde(default)]
    pub payload: String,
}

/// The actor's compact result, as the agent will see it.
///
/// `Deserialize` as well as `Serialize` so the out-of-process shim reads back *this*
/// type rather than a private copy of the field names — the only thing pinning the two
/// halves of that contract together.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct VerbResponse {
    pub ok: bool,
    pub output: String,
}

#[cfg(not(feature = "mcp-transport"))]
async fn run_verb(
    State(_state): State<ControlApiState>,
    Json(_body): Json<VerbRequest>,
) -> Result<Json<VerbResponse>, StatusCode> {
    Err(StatusCode::NOT_IMPLEMENTED)
}

/// `/control/verb`: run one actor verb for a session, in **this** process.
///
/// The counterpart to `/control/mcp-endpoint`. That one suits an IDE that launches the
/// agent and can put a per-session URL in its launch flags; this one suits an agent
/// that was already running when we met it, whose MCP server is a short-lived child
/// process with no way to be told a URL in advance (see the daemon's `mcp`
/// subcommand). Both end at the same actor, so the verb is gated and audited once.
///
/// It inherits the actor's timing, which for an approval-bearing verb means **it does
/// not return until the operator decides**. That is the point — `present_plan` is
/// supposed to block — so any client of this route needs a read timeout to match.
#[cfg(feature = "mcp-transport")]
async fn run_verb(
    State(state): State<ControlApiState>,
    Json(body): Json<VerbRequest>,
) -> Result<Json<VerbResponse>, StatusCode> {
    let Some(host) = state.mcp.as_ref() else {
        return Err(StatusCode::NOT_IMPLEMENTED);
    };
    let request = moonlight_domain::ports::mcp::ActorRequest {
        session: SessionId::new(body.session_id),
        verb: body.verb,
        payload: body.payload,
    };
    match host.actor().run(&request).await {
        Ok(result) => Ok(Json(VerbResponse {
            ok: result.ok,
            output: result.compact_output,
        })),
        // A refusal is a real answer the agent must read, not a transport failure —
        // returning an error status would hide the reason it was denied.
        Err(error) => Ok(Json(VerbResponse {
            ok: false,
            output: error.to_string(),
        })),
    }
}

/// Bind an MCP endpoint for a session and return its URL.
///
/// An IDE calls this *before* launching, because the URL has to go in the launch
/// arguments. The daemon cannot bind it in advance: the endpoint is bound to a session
/// id, and the id is minted by whoever starts the session.
#[cfg(not(feature = "mcp-transport"))]
async fn mcp_endpoint(
    State(_state): State<ControlApiState>,
    Json(_body): Json<McpEndpointRequest>,
) -> Result<Json<McpEndpointResponse>, StatusCode> {
    Err(StatusCode::NOT_IMPLEMENTED)
}

#[cfg(feature = "mcp-transport")]
async fn mcp_endpoint(
    State(state): State<ControlApiState>,
    Json(body): Json<McpEndpointRequest>,
) -> Result<Json<McpEndpointResponse>, StatusCode> {
    let Some(host) = state.mcp.as_ref() else {
        // This host serves no MCP. Saying so plainly beats returning a URL that
        // answers nothing.
        return Err(StatusCode::NOT_IMPLEMENTED);
    };
    match host.spawn_for(SessionId::new(body.session_id)).await {
        Ok(url) => Ok(Json(McpEndpointResponse { url })),
        Err(error) => {
            tracing::warn!(%error, "could not bind an MCP endpoint for the session");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Stream engine events to a client.
///
/// This is what lets an IDE stop running its own engine. Previously every client folded
/// an in-process `EventBus`, so two processes each reached their own answer about what
/// was happening — and only whichever one won the socket race actually governed
/// anything, while the other showed a confident, wrong picture.
///
/// Events are sent as JSON lines over SSE. A client that misses some has a stale view
/// until the next event, which is why the fleet and review endpoints remain the
/// authority for state: this stream is a change notification, not the record.
async fn events(
    State(state): State<ControlApiState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let mut rx = state.events.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    match serde_json::to_string(&event) {
                        Ok(json) => yield Ok(Event::default().data(json)),
                        Err(error) => {
                            tracing::warn!(%error, "could not serialise an engine event");
                        }
                    }
                }
                // The client's view is now incomplete; it re-reads the authoritative
                // endpoints rather than being handed a silently partial history.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "event stream lagged for a client");
                    yield Ok(Event::default().event("lagged").data(missed.to_string()));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// A hold currently waiting on an operator.
#[derive(Debug, Serialize)]
pub struct PendingApprovalView {
    pub session_id: String,
    pub what: String,
    pub plan: Option<String>,
    pub mcp_tool: Option<String>,
    pub since_ms: u64,
}

/// Holds outstanding right now.
///
/// `ApprovalRequested` goes out on the event stream once, when the hold starts. A
/// client that connects after that — an editor window opened or reloaded mid-hold —
/// has missed it, and because a client-facing hold waits indefinitely (see
/// `apps/daemon`), nothing would surface the blocked session again. This is the
/// catch-up read that makes a missed event recoverable rather than terminal.
async fn pending_approvals(State(state): State<ControlApiState>) -> Json<Vec<PendingApprovalView>> {
    let mut rows: Vec<PendingApprovalView> = state
        .pending
        .outstanding()
        .into_iter()
        .map(|(session, held)| PendingApprovalView {
            session_id: session.as_str().to_string(),
            what: held.what,
            plan: held.plan,
            mcp_tool: held.mcp_tool,
            since_ms: held.since_ms,
        })
        .collect();
    // Longest-waiting first: that is the one closest to an operator having forgotten it.
    rows.sort_by_key(|row| row.since_ms);
    Json(rows)
}

/// An operator's verdict on a held action.
#[derive(Debug, Deserialize)]
pub struct VerdictRequest {
    pub session_id: String,
    /// `true` releases the hook; `false` denies it.
    pub approve: bool,
    /// Why it was denied. This is the feedback the agent acts on, so a bare refusal
    /// tells it nothing — the caller should supply one.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Answer a held approval.
///
/// A client cannot resolve the hold itself: the hook is waiting on a oneshot in this
/// process. Without this endpoint an IDE could only send commands the supervisor
/// handles, leaving every held plan and danger-zone action unanswerable from anywhere
/// but the process that happens to own the socket.
async fn verdict(
    State(state): State<ControlApiState>,
    Json(body): Json<VerdictRequest>,
) -> StatusCode {
    let session = SessionId::new(body.session_id);
    let command = if body.approve {
        Command::ApproveAction {
            session: session.clone(),
        }
    } else {
        Command::DenyAction {
            session: session.clone(),
            reason: body
                .reason
                .unwrap_or_else(|| "Denied by the operator.".to_string()),
        }
    };

    // Forwarded only if nothing was held — the supervisor's own approve/deny path then
    // runs, which is what delivers feedback to a session that was not blocked on a hook.
    if let Some(command) = crate::adapters::route_approval(&state.pending, &state.events, command) {
        if state.commands.send(command).is_err() {
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }
    StatusCode::ACCEPTED
}

pub fn router(state: ControlApiState) -> Router {
    Router::new()
        .route("/control/health", get(health))
        .route("/control/gating-status", get(gating_status))
        .route("/control/usage", get(usage))
        .route("/control/review-queue", get(review_queue))
        .route("/control/review-queue/accept", post(accept))
        .route("/control/review-queue/reject", post(reject))
        .route("/control/discoverable-sessions", get(discoverable_sessions))
        .route("/control/adopt", post(adopt))
        .route("/control/review/baseline", get(baseline))
        .route("/control/review/comments", get(comments).post(add_comment))
        .route("/control/review/comments/update", post(update_comment))
        .route("/control/review/comments/delete", post(delete_comment))
        .route("/control/review/submit", post(submit_review))
        .route("/control/review/delivered", post(mark_delivered))
        .route("/control/review/ignore", get(ignored).post(set_ignored))
        .route("/control/events", get(events))
        .route("/control/verdict", post(verdict))
        .route("/control/pending-approvals", get(pending_approvals))
        .route("/control/mcp-endpoint", post(mcp_endpoint))
        .route("/control/verb", post(run_verb))
        .route("/control/phase", post(set_phase))
        .route("/control/phase/advance", post(advance_phase))
        .with_state(state)
}

/// Serve the control API on `listener` until it errors or the process exits.
pub async fn serve(
    state: ControlApiState,
    listener: tokio::net::TcpListener,
) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

/// Liveness, for a client deciding whether to start a daemon.
///
/// Its own route rather than reusing `/control/gating-status`: that one reads the
/// hook registration off disk on every call, and this is polled. Answering needs no
/// state at all, which is the point — it stays true while the rest is still warming
/// up.
///
/// `api` is what a client actually checks, and it is not the package version: the
/// IDEs and the daemon version independently, so comparing `version` would report
/// skew between builds that agree perfectly. Bump [`CONTROL_API_REVISION`] when an
/// endpoint a client depends on is added.
///
/// This matters concretely: an installed daemon predating the MCP migration serves
/// `/control/gating-status` happily but has no `/control/mcp-endpoint`, so every
/// session it is asked about launches with no `moonlight` verbs at all — and the only
/// symptom was one line in a log.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "api": CONTROL_API_REVISION,
    }))
}

/// Revision of the control API's surface.
///
/// 1 — `/control/health`, `/control/events`, `/control/verdict`,
/// `/control/mcp-endpoint`: everything a daemon-hosted IDE needs to stop running its
/// own engine.
pub const CONTROL_API_REVISION: u32 = 1;

async fn gating_status(State(state): State<ControlApiState>) -> Json<Vec<HookStatusEntry>> {
    Json((state.hooks_status)())
}

/// Account quota + per-session context/uptime.
///
/// The two halves have very different costs, so they are gathered differently: the
/// session rows are a cheap directory read on every request, while the quota is a
/// network fetch behind [`QUOTA_TTL`]. Both run on the blocking pool — `curl` and
/// the disk scan would otherwise stall the async runtime that is also serving the
/// hook gate, and a held hook is a stalled agent.
async fn usage(State(state): State<ControlApiState>) -> Json<UsageResponse> {
    let quota = state.cached_quota().await;
    let loaded = tokio::task::spawn_blocking(moonlight_core::obs::load_all)
        .await
        .unwrap_or_default();
    // Titles come from the managed store; an unadopted or unknown session still gets
    // a row (its id is what the client matched on), just without a name.
    let titles: std::collections::HashMap<String, Option<String>> = state
        .sessions
        .all_managed()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.id.as_str().to_string(), s.title))
        .collect();
    Json(UsageResponse {
        quota,
        sessions: session_usage_rows(loaded, &titles),
    })
}

/// Map the loaded snapshots onto the wire type, freshest first. Split from the
/// handler (which reads the real `<support>/obs/`) so the mapping is testable.
fn session_usage_rows(
    loaded: Vec<(String, moonlight_core::obs::SessionObs)>,
    titles: &std::collections::HashMap<String, Option<String>>,
) -> Vec<SessionUsage> {
    let mut rows: Vec<SessionUsage> = loaded
        .into_iter()
        .map(|(session_id, obs)| SessionUsage {
            ctx_pct: obs.ctx_pct(),
            title: titles.get(&session_id).cloned().flatten(),
            session_id,
            model: obs.model,
            persona: obs.persona,
            session_ms: obs.session_ms,
            session_uptime: moonlight_core::obs::fmt_dur(obs.session_ms),
            ctx_tokens: obs.ctx_tokens,
            ctx_limit: obs.ctx_limit,
            cost_usd: obs.cost_usd,
            updated_ms: obs.updated_ms,
        })
        .collect();
    // Freshest first, so a client showing one row shows the session in use.
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_ms));
    rows
}

impl ControlApiState {
    /// The account quota, re-fetched at most once per [`QUOTA_TTL`]. A failed fetch
    /// is cached as `None` for the same interval on purpose: retrying a `curl` that
    /// just failed, on every poll, is how a missing credential turns into a process
    /// spawn per second.
    async fn cached_quota(&self) -> Option<AccountUsage> {
        if let Ok(guard) = self.quota.lock() {
            if let Some((at, quota)) = guard.as_ref() {
                if at.elapsed() < QUOTA_TTL {
                    return quota.clone().map(account_usage);
                }
            }
        }
        let fetched = tokio::task::spawn_blocking(moonlight_core::obs::quota)
            .await
            .unwrap_or(None);
        if let Ok(mut guard) = self.quota.lock() {
            *guard = Some((std::time::Instant::now(), fetched.clone()));
        }
        fetched.map(account_usage)
    }
}

/// Render a [`Quota`](moonlight_core::obs::Quota) for the wire, resolving the reset
/// countdown at response time.
fn account_usage(q: moonlight_core::obs::Quota) -> AccountUsage {
    AccountUsage {
        five_hour_pct: q.five_hour_pct,
        five_hour_resets_at: q.five_hour_resets_at,
        five_hour_resets_in: q
            .five_hour_resets_at
            .and_then(moonlight_core::obs::fmt_reset_in),
        weekly_pct: q.weekly_pct,
        sonnet_pct: q.sonnet_pct,
    }
}

async fn review_queue(State(state): State<ControlApiState>) -> Json<Vec<ReviewQueueItem>> {
    Json(collect_review_queue(&state.sessions, &state.changes))
}

/// Every unreviewed file across adopted managed sessions, with its diff. Kept free
/// of the HTTP types so it's unit-testable against fake stores.
fn collect_review_queue(
    sessions: &Arc<dyn ManagedSessionStore>,
    changes: &Arc<dyn SessionChangeStore>,
) -> Vec<ReviewQueueItem> {
    let Ok(managed) = sessions.all_managed() else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for session in managed {
        if !session.adopted {
            continue;
        }
        let Ok(touched) = changes.touched_paths(&session.id) else {
            continue;
        };
        for touch in touched.into_iter().filter(|t| !t.reviewed) {
            let diff = render_diff(session.root.as_deref(), &session.id, &touch.path, changes);
            items.push(ReviewQueueItem {
                session_id: session.id.as_str().to_string(),
                session_title: session.title.clone(),
                touches: touch.touches,
                tool: touch.tool,
                created: touch.created,
                from_head: touch.from_head,
                file_path: touch.path,
                diff,
            });
        }
    }
    items
}

/// A unified diff of `path` against `session`'s recorded baseline, or empty when
/// there's no repo root, no baseline, or the current file can't be read.
fn render_diff(
    root: Option<&str>,
    session: &SessionId,
    path: &str,
    changes: &Arc<dyn SessionChangeStore>,
) -> String {
    let Some(root) = root else {
        return String::new();
    };
    let Ok(Some(baseline)) = changes.baseline(session, path) else {
        return String::new();
    };
    let Some(before) = baseline.text() else {
        return String::new();
    };
    let Ok(after) = std::fs::read_to_string(std::path::Path::new(root).join(path)) else {
        return String::new();
    };
    similar::TextDiff::from_lines(before, &after)
        .unified_diff()
        .header(path, path)
        .to_string()
}

async fn accept(
    State(state): State<ControlApiState>,
    Json(body): Json<AcceptRequest>,
) -> StatusCode {
    let session = SessionId::new(body.session_id);
    let _ = state.changes.mark_reviewed(
        &session,
        &body.path,
        Some(Timestamp::from_millis(now_millis())),
    );
    StatusCode::NO_CONTENT
}

async fn reject(
    State(state): State<ControlApiState>,
    Json(body): Json<RejectRequest>,
) -> Json<RejectResponse> {
    let session = SessionId::new(body.session_id);
    let feedback = Feedback {
        session_id: session.clone(),
        message: body.message,
        origin: FeedbackOrigin::HunkRejection,
    };
    // Ask before sending: the command channel only tells us the engine loop is
    // alive, not that anything downstream can deliver steering to the agent.
    let mut delivery = (state.feedback_delivery)();
    // Best-effort: an unread channel (engine loop gone) must not fail the HTTP call.
    if state
        .commands
        .send(Command::RejectHunk { feedback })
        .is_err()
    {
        delivery = FeedbackDelivery::Undeliverable {
            reason: "the engine command channel is closed — the backend is shutting down".into(),
        };
    }
    // Drop the file from the queue until the session touches it again — otherwise
    // the same rejected file would nag on every poll while the agent is still
    // acting on the feedback. Done even when the message couldn't be delivered, so
    // the operator's review decision isn't silently undone.
    let reviewed = state
        .changes
        .mark_reviewed(
            &session,
            &body.path,
            Some(Timestamp::from_millis(now_millis())),
        )
        .is_ok();
    Json(RejectResponse {
        reviewed,
        feedback: delivery,
    })
}

/// Files `session` has written and nobody has reviewed. Counts only — no diff is
/// rendered, which is what makes this cheap enough to ride the client's poll.
/// A store error counts as zero: an indicator that under-reports is a missing badge,
/// while one that reports a number it could not read is a lie about the tree.
fn unreviewed_count(changes: &Arc<dyn SessionChangeStore>, session: &SessionId) -> usize {
    changes
        .touched_paths(session)
        .map(|touched| touched.iter().filter(|t| !t.reviewed).count())
        .unwrap_or(0)
}

async fn discoverable_sessions(
    State(state): State<ControlApiState>,
) -> Json<Vec<DiscoverableSession>> {
    let mut sessions: Vec<DiscoverableSession> = state
        .fleet
        .all()
        .into_iter()
        .map(|s| DiscoverableSession {
            unreviewed_files: unreviewed_count(&state.changes, &s.id),
            session_id: s.id.as_str().to_string(),
            title: s.title,
            root: s.attached_path,
            adopted: s.adopted,
            status: s.status,
            phase: s.phase,
        })
        .collect();
    sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    Json(sessions)
}

/// Adopt a session detection has found — the operator opting it into MoonlightCode
/// governance (phase/trust gating) from outside the desktop app.
///
/// A session the fleet has never seen is reported as `404`, not silently accepted:
/// the engine drops `SetAdopted` for a session that isn't in the fleet, so a `204`
/// there would claim a governance change that never happened. The client re-polls
/// `/control/discoverable-sessions` and retries once detection has caught up.
async fn adopt(State(state): State<ControlApiState>, Json(body): Json<AdoptRequest>) -> StatusCode {
    let session = SessionId::new(body.session_id);
    if !state.fleet.knows(&session) {
        return StatusCode::NOT_FOUND;
    }
    if state
        .commands
        .send(Command::SetAdopted {
            session,
            adopted: true,
        })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::ACCEPTED
}

/// Move a session to an explicit phase. This is an operator override, so the engine
/// **pins** it — auto-advance and detection reconcile leave it alone until released.
///
/// The reason this endpoint exists: adoption subjects a session to the gate, and a
/// freshly adopted session sits in [`Phase::Plan`], where project writes are denied.
/// Without a way to leave Plan from outside the desktop app, adopting from an editor
/// silently freezes the session with no escape hatch.
async fn set_phase(
    State(state): State<ControlApiState>,
    Json(body): Json<SetPhaseRequest>,
) -> StatusCode {
    let session = SessionId::new(body.session_id);
    if !state.fleet.knows(&session) {
        return StatusCode::NOT_FOUND;
    }
    if state
        .commands
        .send(Command::SetPhase {
            session,
            phase: body.phase,
        })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::ACCEPTED
}

/// Advance to the next phase and return the session to auto (clears any pin) — the
/// same operator-confirmed advancement the cockpit's phase stepper emits.
async fn advance_phase(
    State(state): State<ControlApiState>,
    Json(body): Json<AdvancePhaseRequest>,
) -> StatusCode {
    let session = SessionId::new(body.session_id);
    if !state.fleet.knows(&session) {
        return StatusCode::NOT_FOUND;
    }
    if state
        .commands
        .send(Command::AdvancePhase { session })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::ACCEPTED
}

#[derive(Debug, Deserialize)]
pub struct BaselineQuery {
    pub session: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    pub session: String,
}

/// The "before" side of one reviewed file. Query: `?session=<id>&path=<abs path>`.
///
/// The plugin needs the baseline *content* to render a real diff editor; the
/// unified-diff text on a queue item can only be shown as a blob.
async fn baseline(
    State(state): State<ControlApiState>,
    Query(q): Query<BaselineQuery>,
) -> Result<Json<BaselineView>, StatusCode> {
    let session = SessionId::new(q.session);
    match state.changes.baseline(&session, &q.path) {
        Ok(Some(Baseline::Content(text))) => Ok(Json(BaselineView::Content { text })),
        Ok(Some(Baseline::FromHead(text))) => Ok(Json(BaselineView::FromHead { text })),
        Ok(Some(Baseline::Created)) => Ok(Json(BaselineView::Created)),
        Ok(Some(Baseline::Unavailable { reason })) => Ok(Json(BaselineView::Unavailable {
            reason: format!("{reason:?}"),
        })),
        // No baseline row at all: this session never touched the file.
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Whether `c`'s anchored line still reads as it did when the comment was written.
/// Only line-scoped comments are anchored; anything else can't drift.
fn is_outdated(c: &ReviewComment) -> bool {
    let Some(anchor) = c.anchor_text.as_deref() else {
        return false;
    };
    if !c.scope.is_line() || c.start_line == 0 {
        return false;
    }
    // A `Before`-side comment is anchored to the baseline, which is immutable — it
    // is the pre-image, so it cannot drift.
    if matches!(c.side, DiffSide::Before) {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&c.path) else {
        // File gone: the comment certainly isn't about what's there now.
        return true;
    };
    text.lines()
        .nth(c.start_line.saturating_sub(1) as usize)
        .map(|line| line != anchor)
        .unwrap_or(true)
}

fn to_view(c: ReviewComment) -> ReviewCommentView {
    // Computed before the struct consumes `c`.
    let outdated = is_outdated(&c);
    ReviewCommentView {
        id: c.id,
        scope: c.scope,
        path: c.path,
        side: c.side,
        start_line: c.start_line,
        end_line: c.end_line,
        body: c.body,
        anchor_text: c.anchor_text,
        sent: c.sent_at.is_some(),
        resolved: c.resolved_at.is_some(),
        outdated,
        author: c.author,
        parent_id: c.parent_id,
    }
}

async fn comments(
    State(state): State<ControlApiState>,
    Query(q): Query<SessionQuery>,
) -> Json<Vec<ReviewCommentView>> {
    let session = SessionId::new(q.session);
    Json(
        state
            .changes
            .comments(&session)
            .unwrap_or_default()
            .into_iter()
            .map(to_view)
            .collect(),
    )
}

/// Persist a comment the moment it is written — closing the editor must not lose a
/// review in progress, which is why this is a write rather than client-side state.
async fn add_comment(
    State(state): State<ControlApiState>,
    Json(body): Json<AddCommentRequest>,
) -> Result<Json<ReviewCommentView>, StatusCode> {
    let session = SessionId::new(body.session_id);
    let now = now_millis();
    // A reply belongs to the thread it answers, at the place that thread is anchored.
    // Resolved server-side from the stored root rather than trusted from the client:
    // two surfaces are writing here, and an anchor that could be overridden per reply
    // is an anchor that will eventually disagree with its own thread.
    let parent = match body.parent_id.as_deref() {
        None => None,
        Some(parent_id) => Some(
            state
                .changes
                .comments(&session)
                .unwrap_or_default()
                .into_iter()
                .find(|c| c.id == parent_id)
                .ok_or(StatusCode::NOT_FOUND)?,
        ),
    };
    let comment = match &parent {
        Some(root) => ReviewComment {
            // Time-ordered id like the audit log's, so comments sort by when they were
            // written even within the same millisecond.
            id: format!("{now}-{}", state.next_comment_seq()),
            session_id: session,
            // Threads stay one level deep: answering an answer joins the conversation
            // it belongs to instead of starting a branch nothing can render.
            parent_id: Some(root.thread_id().to_string()),
            author: body.author.unwrap_or(CommentAuthor::Operator),
            body: body.body,
            at: Timestamp::from_millis(now),
            sent_at: None,
            // Resolution is a property of the thread, held by its root.
            resolved_at: None,
            scope: root.scope,
            path: root.path.clone(),
            side: root.side,
            start_line: root.start_line,
            end_line: root.end_line,
            anchor_text: root.anchor_text.clone(),
        },
        None => ReviewComment {
            id: format!("{now}-{}", state.next_comment_seq()),
            session_id: session,
            scope: body.scope,
            path: body.path,
            side: body.side,
            start_line: body.start_line,
            end_line: body.end_line,
            body: body.body,
            anchor_text: body.anchor_text,
            author: body.author.unwrap_or(CommentAuthor::Operator),
            parent_id: None,
            at: Timestamp::from_millis(now),
            sent_at: None,
            resolved_at: None,
        },
    };
    state
        .changes
        .add_comment(&comment)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(to_view(comment)))
}

/// Edit a comment's body and/or its resolved state.
///
/// Changing the body clears `sent_at`, which is what puts a corrected comment back
/// into the next batch — otherwise the session keeps acting on the version that
/// landed wrong.
///
/// Resolving is a **thread** operation: asking to resolve a reply settles the
/// conversation it belongs to, because half a settled thread is not a state anything
/// can show. The edited comment is still what comes back, so a client that resolved
/// from a reply sees its own request answered.
async fn update_comment(
    State(state): State<ControlApiState>,
    Json(body): Json<UpdateCommentRequest>,
) -> Result<Json<ReviewCommentView>, StatusCode> {
    let session = SessionId::new(body.session_id);
    let all = state.changes.comments(&session).unwrap_or_default();
    let mut existing = all
        .iter()
        .find(|c| c.id == body.id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    if let Some(text) = body.body {
        if text != existing.body {
            existing.body = text;
            existing.sent_at = None;
        }
    }
    if let Some(resolved) = body.resolved {
        let at = resolved.then(|| Timestamp::from_millis(now_millis()));
        // The root carries the thread's resolution. When the request named a reply,
        // stamp the root too — and let a failure there fail the call rather than
        // leaving a thread resolved at one end and open at the other.
        if let Some(root) = all.iter().find(|c| c.id == existing.thread_id()) {
            if root.id != existing.id {
                let root = ReviewComment {
                    resolved_at: at,
                    ..root.clone()
                };
                state
                    .changes
                    .update_comment(&root)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
        }
        existing.resolved_at = at;
    }
    match state.changes.update_comment(&existing) {
        Ok(true) => Ok(Json(to_view(existing))),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn delete_comment(
    State(state): State<ControlApiState>,
    Json(body): Json<DeleteCommentRequest>,
) -> StatusCode {
    match state.changes.delete_comment(&body.id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Deliver every unsent, unresolved comment as **one** message — the point of
/// batching: the session gets a coherent review to act on, not a stream of pokes.
///
/// Formatted by [`moonlight_domain::changes::review_message`], the same function the
/// desktop panel uses, so the text can't depend on which window reviewed.
async fn submit_review(
    State(state): State<ControlApiState>,
    Json(body): Json<SubmitReviewRequest>,
) -> Result<Json<SubmitReviewResponse>, StatusCode> {
    let session = SessionId::new(body.session_id);
    let all = state.changes.comments(&session).unwrap_or_default();
    // What the agent owes an answer to: the reviewer's messages, undelivered and
    // unsettled. Its own replies are excluded — a session handed its own words back
    // answers itself, and the thread never ends.
    let pending: Vec<ReviewComment> = moonlight_domain::changes::pending_feedback(&all)
        .into_iter()
        .cloned()
        .collect();
    if pending.is_empty() {
        return Err(StatusCode::NO_CONTENT);
    }
    // Rendered from the *whole* record, not just the pending slice: a thread the agent
    // already answered must arrive carrying that answer, or it re-argues a point it
    // has already made. `review_message` picks the threads actually waiting on it.
    let body = moonlight_domain::changes::review_message(&all);
    let body = format!("{body}{}", reply_instructions(&session, &all));
    // Persist the review beside the session's other MoonlightCode state. `.moonlight/`
    // carries a `*` gitignore, so this never dirties the repo being reviewed.
    let review_path = session_root(&state, &session).and_then(|root| {
        write_review_file(&root, session.as_str(), &body)
            .map_err(|err| tracing::warn!(error = %err, "couldn't write the review file"))
            .ok()
    });
    // What gets delivered is a pointer, not the review. Injected context is capped
    // (10k chars), and a real review of a dozen files blows through that — so the
    // message stays short and the file carries the content.
    let message = match &review_path {
        Some(path) => format!(
            "A code review of your changes is ready: {path}\n             Read that file and address each comment. It lists {} comment(s),              widest scope first.",
            pending.len()
        ),
        None => body,
    };
    let mut delivery = (state.feedback_delivery)();
    if state
        .commands
        .send(Command::SubmitReview {
            session: session.clone(),
            message,
        })
        .is_err()
    {
        delivery = FeedbackDelivery::Undeliverable {
            reason: "the engine command channel is closed — the backend is shutting down".into(),
        };
    }
    let comment_count = pending.len();
    let comment_ids: Vec<String> = pending.iter().map(|c| c.id.clone()).collect();
    // Stamp only what actually went. An undelivered review stays pending so the
    // operator can send it again once a delivery path exists, rather than losing it.
    if matches!(delivery, FeedbackDelivery::Queued) {
        let ids: Vec<String> = pending.into_iter().map(|c| c.id).collect();
        let _ = state
            .changes
            .mark_comments_sent(&ids, Timestamp::from_millis(now_millis()));
    }
    Ok(Json(SubmitReviewResponse {
        comment_count,
        comment_ids,
        review_path,
        feedback: delivery,
    }))
}

/// The repo root recorded for `session`, if it is a managed session with one.
fn session_root(state: &ControlApiState, session: &SessionId) -> Option<String> {
    state
        .sessions
        .all_managed()
        .ok()?
        .into_iter()
        .find(|m| &m.id == session)
        .and_then(|m| m.root)
}

/// How the session answers a comment, appended to the delivered review.
///
/// This is what makes a review a conversation rather than a list of orders: until
/// the agent is told the endpoint and the thread ids, its only options are silent
/// compliance or arguing in the chat, where the objection and the answer end up in
/// different places. Written here rather than in the domain's `review_message`
/// because it is transport-specific — it names *this* control API.
///
/// Only threads awaiting an answer are listed, so the agent is never invited to
/// reply to settled business.
fn reply_instructions(session: &SessionId, all: &[ReviewComment]) -> String {
    let waiting: Vec<(&ReviewComment, Vec<&ReviewComment>)> =
        moonlight_domain::changes::threads(all)
            .into_iter()
            .filter(|(root, replies)| {
                !root.is_resolved()
                    && std::iter::once(*root)
                        .chain(replies.iter().copied())
                        .any(|c| c.author.is_feedback() && c.sent_at.is_none())
            })
            .collect();
    if waiting.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\nAnswering\n\
         ---------\n\
         You can reply to any comment instead of only acting on it — to ask what was\n\
         meant, to push back, or to say what you did. Replies join the thread the\n\
         reviewer is reading, and they are not delivered back to you as new feedback.\n\n\
         Read the control API's port from ~/.moonlight/control.json, then POST to\n\
         http://127.0.0.1:<port>/control/review/comments with:\n\n\
           {\"session_id\": \"",
    );
    out.push_str(session.as_str());
    out.push_str(
        "\", \"parent_id\": \"<thread id below>\",\n\
         \x20   \"author\": \"Agent\", \"body\": \"<your reply>\",\n\
         \x20   \"scope\": \"Line\", \"side\": \"After\"}\n\n\
         Threads awaiting you:\n",
    );
    for (root, _) in waiting {
        out.push_str(&format!("  {}  ({})\n", root.id, root.anchor()));
    }
    out
}

/// Write `body` to `<root>/.moonlight/reviews/<timestamp>-<session>.md` and return
/// the path. Timestamped rather than overwritten: a review is a record of what was
/// asked and when, and the previous one is what explains the code the agent then
/// wrote.
fn write_review_file(root: &str, session: &str, body: &str) -> std::io::Result<String> {
    let dir = std::path::Path::new(root)
        .join(moonlight_control::MOONLIGHT_DIR)
        .join("reviews");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}-{session}.md", now_millis()));
    std::fs::write(&path, body)?;
    Ok(path.to_string_lossy().into_owned())
}

/// Record that a client delivered a review it submitted. Separate from `submit`
/// because the two can be done by different parties: the backend delivers when it
/// owns the session's terminal, and an editor delivers when it does.
async fn mark_delivered(
    State(state): State<ControlApiState>,
    Json(body): Json<MarkDeliveredRequest>,
) -> StatusCode {
    let session = SessionId::new(body.session_id);
    match state
        .changes
        .mark_comments_sent(&body.comment_ids, Timestamp::from_millis(now_millis()))
    {
        Ok(()) => {
            tracing::info!(
                session = %session,
                count = body.comment_ids.len(),
                "review delivered by a client"
            );
            StatusCode::NO_CONTENT
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn ignored(
    State(state): State<ControlApiState>,
    Query(q): Query<SessionQuery>,
) -> Json<Vec<String>> {
    let session = SessionId::new(q.session);
    Json(state.changes.ignored_paths(&session).unwrap_or_default())
}

/// Ignore a file (or a folder prefix) for this session's review pass.
async fn set_ignored(
    State(state): State<ControlApiState>,
    Json(body): Json<SetIgnoredRequest>,
) -> StatusCode {
    let session = SessionId::new(body.session_id);
    match state
        .changes
        .set_ignored(&session, &body.path, body.ignored)
    {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use moonlight_domain::agent::AgentKind;
    use moonlight_domain::audit::AuditEntry;
    use moonlight_domain::changes::{Baseline, ChangeTool, FileTouch, ReviewComment, TouchedPath};
    use moonlight_domain::errors::StoreError;
    use moonlight_domain::phase::Phase;
    use moonlight_domain::ports::store::{ManagedSession, ManagedStateUpdate};
    use moonlight_domain::session::Mode;
    use moonlight_domain::trust::TrustTier;

    #[test]
    fn usage_rows_carry_titles_and_sort_freshest_first() {
        use moonlight_core::obs::SessionObs;
        let loaded = vec![
            (
                "stale".to_string(),
                SessionObs {
                    model: "Opus 5".into(),
                    session_ms: 4_320_000,
                    ctx_tokens: 100_000,
                    ctx_limit: 1_000_000,
                    updated_ms: 10,
                    ..Default::default()
                },
            ),
            (
                "fresh".to_string(),
                SessionObs {
                    model: "Sonnet 5".into(),
                    ctx_used_pct: Some(31),
                    updated_ms: 20,
                    ..Default::default()
                },
            ),
        ];
        let titles = HashMap::from([("fresh".to_string(), Some("Refactor the gate".to_string()))]);
        let rows = session_usage_rows(loaded, &titles);
        // The row a client would show first is the session most recently observed.
        assert_eq!(rows[0].session_id, "fresh");
        assert_eq!(rows[0].title.as_deref(), Some("Refactor the gate"));
        // CC's own context figure wins when it reports one.
        assert_eq!(rows[0].ctx_pct, Some(31));
        // A session absent from the managed store still gets a row, just unnamed.
        assert_eq!(rows[1].title, None);
        assert_eq!(rows[1].ctx_pct, Some(10)); // 100k ÷ 1M
        assert_eq!(rows[1].session_uptime, "1h12m");
    }

    #[test]
    fn account_usage_renders_the_reset_countdown() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 2 * 3600
            + 14 * 60
            // Half a minute of slack so the countdown can't tip to 2h13m between
            // computing this and rendering it.
            + 30;
        let wire = account_usage(moonlight_core::obs::Quota {
            five_hour_pct: Some(42),
            five_hour_resets_at: Some(future),
            weekly_pct: Some(18),
            sonnet_pct: None,
        });
        assert_eq!(wire.five_hour_pct, Some(42));
        assert_eq!(wire.five_hour_resets_in.as_deref(), Some("2h14m"));
        // An unknown window stays unknown — never rendered as 0%.
        assert_eq!(wire.sonnet_pct, None);
    }

    struct FakeSessions(Vec<ManagedSession>);
    impl ManagedSessionStore for FakeSessions {
        fn upsert_managed(&self, _s: &ManagedSession) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
        fn update_managed_state(&self, _u: &ManagedStateUpdate) -> Result<bool, StoreError> {
            unreachable!("not exercised by this test")
        }
        fn set_conversation_id(&self, _id: &SessionId, _cid: &str) -> Result<bool, StoreError> {
            unreachable!("not exercised by this test")
        }
        fn managed(&self, _id: &SessionId) -> Result<Option<ManagedSession>, StoreError> {
            unreachable!("not exercised by this test")
        }
        fn all_managed(&self) -> Result<Vec<ManagedSession>, StoreError> {
            Ok(self.0.clone())
        }
        fn remove_managed(&self, _id: &SessionId) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
        fn append_audit(&self, _e: &AuditEntry) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
        fn recent_audit(&self, _s: &SessionId, _n: usize) -> Result<Vec<AuditEntry>, StoreError> {
            unreachable!("not exercised by this test")
        }
    }

    #[derive(Default)]
    struct FakeChanges {
        touched: HashMap<String, Vec<TouchedPath>>,
        baselines: HashMap<(String, String), Baseline>,
        marked_reviewed: Mutex<Vec<(String, String)>>,
    }
    impl SessionChangeStore for FakeChanges {
        fn record_touch(&self, _t: &FileTouch) -> Result<bool, StoreError> {
            unreachable!("not exercised by this test")
        }
        fn touched_paths(&self, session: &SessionId) -> Result<Vec<TouchedPath>, StoreError> {
            Ok(self
                .touched
                .get(session.as_str())
                .cloned()
                .unwrap_or_default())
        }
        fn baseline(
            &self,
            session: &SessionId,
            path: &str,
        ) -> Result<Option<Baseline>, StoreError> {
            Ok(self
                .baselines
                .get(&(session.as_str().to_string(), path.to_string()))
                .cloned())
        }
        fn touched_counts(&self) -> Result<Vec<(SessionId, u32)>, StoreError> {
            unreachable!("not exercised by this test")
        }
        fn forget_session_changes(&self, _s: &SessionId) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
        fn mark_reviewed(
            &self,
            session: &SessionId,
            path: &str,
            _at: Option<Timestamp>,
        ) -> Result<bool, StoreError> {
            self.marked_reviewed
                .lock()
                .unwrap()
                .push((session.as_str().to_string(), path.to_string()));
            Ok(true)
        }
        fn ignored_paths(&self, _s: &SessionId) -> Result<Vec<String>, StoreError> {
            unreachable!("not exercised by this test")
        }
        fn set_ignored(&self, _s: &SessionId, _p: &str, _v: bool) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
        fn clear_ignored(&self, _s: &SessionId) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
        fn add_comment(&self, _c: &ReviewComment) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
        fn comments(&self, _s: &SessionId) -> Result<Vec<ReviewComment>, StoreError> {
            unreachable!("not exercised by this test")
        }
        fn update_comment(&self, _c: &ReviewComment) -> Result<bool, StoreError> {
            unreachable!("not exercised by this test")
        }
        fn delete_comment(&self, _id: &str) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
        fn mark_comments_sent(&self, _ids: &[String], _at: Timestamp) -> Result<(), StoreError> {
            unreachable!("not exercised by this test")
        }
    }

    fn session(id: &str, root: &str, adopted: bool) -> ManagedSession {
        ManagedSession {
            id: SessionId::new(id),
            root: Some(root.to_string()),
            title: Some(format!("session {id}")),
            mode: Mode::Auto,
            phase: Phase::Review,
            agent: AgentKind::ClaudeCode,
            conversation_id: None,
            trust_tier: TrustTier::Observed,
            adopted,
            paused: false,
            phase_pinned: false,
            hidden: false,
            created_at: Timestamp::from_millis(0),
            last_seen: Timestamp::from_millis(0),
        }
    }

    fn touched(path: &str, reviewed: bool) -> TouchedPath {
        TouchedPath {
            path: path.to_string(),
            touches: 1,
            tool: ChangeTool::Edit,
            created: false,
            from_head: false,
            reviewed,
        }
    }

    #[test]
    fn unreviewed_count_counts_only_what_is_still_unreviewed() {
        let changes: Arc<dyn SessionChangeStore> = Arc::new(FakeChanges {
            touched: HashMap::from([(
                "s1".to_string(),
                vec![
                    touched("a.rs", false),
                    touched("b.rs", false),
                    touched("c.rs", true),
                ],
            )]),
            ..Default::default()
        });
        assert_eq!(unreviewed_count(&changes, &SessionId::new("s1")), 2);
        // A session with nothing in the ledger — every unadopted one — reads zero
        // rather than erroring, so the badge simply does not appear.
        assert_eq!(unreviewed_count(&changes, &SessionId::new("ghost")), 0);
    }

    #[test]
    fn queue_skips_unadopted_sessions_and_reviewed_files() {
        let sessions: Arc<dyn ManagedSessionStore> = Arc::new(FakeSessions(vec![
            session("s1", "/repo", true),
            session("s2", "/repo", false),
        ]));
        let changes: Arc<dyn SessionChangeStore> = Arc::new(FakeChanges {
            touched: HashMap::from([
                (
                    "s1".to_string(),
                    vec![touched("a.txt", false), touched("b.txt", true)],
                ),
                ("s2".to_string(), vec![touched("c.txt", false)]),
            ]),
            ..Default::default()
        });

        let items = collect_review_queue(&sessions, &changes);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].session_id, "s1");
        assert_eq!(items[0].file_path, "a.txt");
    }

    #[test]
    fn diff_renders_against_the_baseline_and_the_file_on_disk() {
        let dir =
            std::env::temp_dir().join(format!("moonlight-control-api-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "line one\nline two\n").unwrap();

        let sessions: Arc<dyn ManagedSessionStore> = Arc::new(FakeSessions(vec![session(
            "s1",
            dir.to_str().unwrap(),
            true,
        )]));
        let changes: Arc<dyn SessionChangeStore> = Arc::new(FakeChanges {
            touched: HashMap::from([("s1".to_string(), vec![touched("a.txt", false)])]),
            baselines: HashMap::from([(
                ("s1".to_string(), "a.txt".to_string()),
                Baseline::Content("line one\n".to_string()),
            )]),
            ..Default::default()
        });

        let items = collect_review_queue(&sessions, &changes);
        assert_eq!(items.len(), 1);
        assert!(items[0].diff.contains("+line two"), "{}", items[0].diff);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Builds state whose steering sink is alive or dead, plus the fakes the reject
    /// path touches.
    fn reject_state(deliverable: bool) -> (ControlApiState, Arc<FakeChanges>) {
        let sessions: Arc<dyn ManagedSessionStore> = Arc::new(FakeSessions(vec![]));
        let changes = Arc::new(FakeChanges::default());
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        // Leak the receiver so the command channel stays open for the duration of
        // the test — we are asserting on feedback delivery, not channel teardown.
        std::mem::forget(command_rx);
        let delivery = move || {
            if deliverable {
                FeedbackDelivery::Queued
            } else {
                FeedbackDelivery::Undeliverable {
                    reason: "no terminal to write into".into(),
                }
            }
        };
        let state = ControlApiState::new(
            commands,
            sessions,
            changes.clone() as Arc<dyn SessionChangeStore>,
            Vec::new,
            delivery,
            Arc::new(BusFleetView::new()),
            EventBus::new(16),
        );
        (state, changes)
    }

    /// A reject on a backend with nowhere to deliver steering must say so. Before
    /// this, the handler returned a bare 204 and the operator had no way to learn
    /// their feedback was dropped — the same fail-open-looks-like-success shape
    /// this control surface exists to avoid.
    #[tokio::test]
    async fn reject_reports_undeliverable_feedback_but_still_reviews_the_file() {
        let (state, changes) = reject_state(false);

        let Json(resp) = reject(
            State(state),
            Json(RejectRequest {
                session_id: "s1".into(),
                path: "a.txt".into(),
                message: "use pathlib".into(),
            }),
        )
        .await;

        assert!(resp.reviewed, "the file must still leave the queue");
        assert!(
            matches!(resp.feedback, FeedbackDelivery::Undeliverable { .. }),
            "got {:?}",
            resp.feedback
        );
        assert_eq!(
            changes.marked_reviewed.lock().unwrap().as_slice(),
            &[("s1".to_string(), "a.txt".to_string())]
        );
    }

    #[tokio::test]
    async fn reject_reports_queued_when_a_steering_sink_is_listening() {
        let (state, _changes) = reject_state(true);

        let Json(resp) = reject(
            State(state),
            Json(RejectRequest {
                session_id: "s1".into(),
                path: "a.txt".into(),
                message: "use pathlib".into(),
            }),
        )
        .await;

        assert!(resp.reviewed);
        assert_eq!(resp.feedback, FeedbackDelivery::Queued);
    }

    /// An actor that always refuses, so the refusal path can be asserted.
    struct DenyingActor;

    #[async_trait::async_trait]
    impl moonlight_domain::ports::mcp::McpActor for DenyingActor {
        async fn run(
            &self,
            _req: &moonlight_domain::ports::mcp::ActorRequest,
        ) -> Result<moonlight_domain::ports::mcp::ActorResult, moonlight_domain::ControlError>
        {
            Err(moonlight_domain::ControlError::Unsupported(
                "phase Plan denies project writes".into(),
            ))
        }
    }

    /// A refusal must arrive as a 200 carrying `ok: false` and the reason.
    ///
    /// This is the whole contract the out-of-process shim depends on: it turns any
    /// non-2xx into "the daemon did not answer", so returning an error status here would
    /// replace "denied because X" with a transport error and send the agent somewhere
    /// else entirely. Nothing pinned it before.
    #[tokio::test]
    async fn a_refused_verb_is_a_200_carrying_its_reason() {
        let state = threaded_state().with_mcp_host(Arc::new(crate::McpHost::with_scope(
            Arc::new(DenyingActor),
            Arc::new(crate::BusPolicyView::new()),
            crate::VerbScope::WorkflowOnly,
        )));

        let Json(body) = run_verb(
            State(state),
            Json(VerbRequest {
                session_id: "s1".into(),
                verb: moonlight_domain::trust::McpVerb::PhaseStatus,
                payload: String::new(),
            }),
        )
        .await
        .expect("a refusal is still a successful response");

        assert!(!body.ok);
        assert!(
            body.output.contains("phase Plan denies project writes"),
            "the reason must survive: {}",
            body.output
        );
    }

    /// State backed by a real store, so the thread tests exercise actual persistence
    /// rather than a fake that agrees with them.
    fn threaded_state() -> ControlApiState {
        let sessions: Arc<dyn ManagedSessionStore> = Arc::new(FakeSessions(vec![]));
        let changes: Arc<dyn SessionChangeStore> =
            Arc::new(moonlight_persistence::Store::open_in_memory().expect("in-memory store"));
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        std::mem::forget(command_rx);
        ControlApiState::new(
            commands,
            sessions,
            changes,
            Vec::new,
            || FeedbackDelivery::Queued,
            Arc::new(BusFleetView::new()),
            EventBus::new(16),
        )
    }

    async fn post_comment(
        state: &ControlApiState,
        parent: Option<&str>,
        author: Option<CommentAuthor>,
        body: &str,
    ) -> ReviewCommentView {
        let Json(view) = add_comment(
            State(state.clone()),
            Json(AddCommentRequest {
                session_id: "s1".into(),
                scope: CommentScope::Line,
                path: "/repo/a.rs".into(),
                side: DiffSide::After,
                start_line: 40,
                end_line: 42,
                body: body.into(),
                anchor_text: Some("    retry(op)".into()),
                parent_id: parent.map(str::to_string),
                author,
            }),
        )
        .await
        .expect("comment accepted");
        view
    }

    /// A reply is about the thread's code, not wherever the client happened to be
    /// looking — so the anchor comes from the root, not from the request.
    #[tokio::test]
    async fn a_reply_inherits_the_anchor_of_the_thread_it_answers() {
        let state = threaded_state();
        let root = post_comment(&state, None, None, "this leaks a handle").await;
        let Json(reply) = add_comment(
            State(state.clone()),
            Json(AddCommentRequest {
                session_id: "s1".into(),
                // Everything anchor-shaped here is wrong on purpose.
                scope: CommentScope::Review,
                path: "/repo/somewhere-else.rs".into(),
                side: DiffSide::Before,
                start_line: 999,
                end_line: 999,
                body: "closed it in the drop impl".into(),
                anchor_text: Some("nonsense".into()),
                parent_id: Some(root.id.clone()),
                author: Some(CommentAuthor::Agent),
            }),
        )
        .await
        .expect("reply accepted");

        assert_eq!(reply.parent_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(reply.author, CommentAuthor::Agent);
        assert_eq!(reply.path, "/repo/a.rs");
        assert_eq!(reply.scope, CommentScope::Line);
        assert_eq!(reply.side, DiffSide::After);
        assert_eq!((reply.start_line, reply.end_line), (40, 42));
    }

    /// Threads stay one level deep: answering an answer joins the conversation rather
    /// than branching it.
    #[tokio::test]
    async fn replying_to_a_reply_joins_the_same_thread() {
        let state = threaded_state();
        let root = post_comment(&state, None, None, "this leaks a handle").await;
        let first = post_comment(&state, Some(&root.id), Some(CommentAuthor::Agent), "fixed").await;
        let second = post_comment(&state, Some(&first.id), None, "not quite").await;
        assert_eq!(second.parent_id.as_deref(), Some(root.id.as_str()));
    }

    /// Half a settled thread is not a state anything can show, so resolving from a
    /// reply settles the conversation it belongs to.
    #[tokio::test]
    async fn resolving_a_reply_settles_the_whole_thread() {
        let state = threaded_state();
        let root = post_comment(&state, None, None, "this leaks a handle").await;
        let reply = post_comment(&state, Some(&root.id), Some(CommentAuthor::Agent), "fixed").await;

        let Json(updated) = update_comment(
            State(state.clone()),
            Json(UpdateCommentRequest {
                session_id: "s1".into(),
                id: reply.id.clone(),
                body: None,
                resolved: Some(true),
            }),
        )
        .await
        .expect("update accepted");
        assert!(updated.resolved);

        let Json(all) = comments(
            State(state.clone()),
            Query(SessionQuery {
                session: "s1".into(),
            }),
        )
        .await;
        let root_now = all.iter().find(|c| c.id == root.id).unwrap();
        assert!(
            root_now.resolved,
            "the root must carry the thread's resolution"
        );
    }

    /// The failure this whole feature exists to prevent: a session being handed its
    /// own answer back as though the reviewer had said it.
    #[tokio::test]
    async fn a_review_of_only_agent_replies_has_nothing_to_deliver() {
        let state = threaded_state();
        let root = post_comment(&state, None, None, "this leaks a handle").await;
        // The reviewer's comment goes out once...
        assert!(submit_review(
            State(state.clone()),
            Json(SubmitReviewRequest {
                session_id: "s1".into(),
            }),
        )
        .await
        .is_ok());
        // ...the agent answers, and that answer is not feedback for anyone.
        post_comment(&state, Some(&root.id), Some(CommentAuthor::Agent), "fixed").await;
        let again = submit_review(
            State(state.clone()),
            Json(SubmitReviewRequest {
                session_id: "s1".into(),
            }),
        )
        .await;
        assert_eq!(again.err(), Some(StatusCode::NO_CONTENT));
    }

    /// A window that opens mid-hold has already missed the `ApprovalRequested` event,
    /// so this endpoint is its only way to learn that a session is sitting blocked —
    /// and answering must take it off the list.
    #[tokio::test]
    async fn an_outstanding_hold_is_listed_until_it_is_answered() {
        let pending = Arc::new(moonlight_control::PendingApprovals::new());
        let state = threaded_state().with_pending(pending.clone());

        let _rx = pending.register_held(
            SessionId::new("s1"),
            moonlight_control::HeldApproval {
                what: "approve plan".into(),
                plan: Some("# Do the thing".into()),
                mcp_tool: None,
                since_ms: 1,
            },
        );

        let Json(rows) = pending_approvals(State(state.clone())).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "s1");
        assert_eq!(rows[0].what, "approve plan");
        assert_eq!(rows[0].plan.as_deref(), Some("# Do the thing"));

        let released = verdict(
            State(state.clone()),
            Json(VerdictRequest {
                session_id: "s1".into(),
                approve: true,
                reason: None,
            }),
        )
        .await;
        assert_eq!(released, StatusCode::ACCEPTED);

        let Json(rows) = pending_approvals(State(state)).await;
        assert!(rows.is_empty(), "an answered hold must stop being offered");
    }

    /// The agent is told how to answer, and which threads are actually open — being
    /// invited to reply to settled business is how a review restarts itself.
    #[test]
    fn reply_instructions_list_only_the_threads_awaiting_an_answer() {
        let session = SessionId::new("s1");
        let open = anchored("/repo/a.rs", 40, None);
        let settled = ReviewComment {
            id: "c2".into(),
            resolved_at: Some(Timestamp::from_millis(9)),
            ..anchored("/repo/b.rs", 7, None)
        };
        let text = reply_instructions(&session, &[open.clone(), settled.clone()]);
        assert!(
            text.contains(&open.id),
            "open thread must be listed:\n{text}"
        );
        assert!(!text.contains("c2"), "settled thread must not be:\n{text}");
        assert!(text.contains("/control/review/comments"), "{text}");
        assert!(text.contains("\"author\": \"Agent\""), "{text}");
        // Nothing waiting → no instructions at all, rather than an empty invitation.
        assert_eq!(reply_instructions(&session, &[settled]), "");
    }

    fn anchored(path: &str, line: u32, anchor: Option<&str>) -> ReviewComment {
        ReviewComment {
            id: "c1".into(),
            session_id: SessionId::new("s1"),
            scope: CommentScope::Line,
            path: path.to_string(),
            side: DiffSide::After,
            start_line: line,
            end_line: line,
            body: "look at this".into(),
            anchor_text: anchor.map(str::to_string),
            author: CommentAuthor::Operator,
            parent_id: None,
            at: Timestamp::from_millis(0),
            sent_at: None,
            resolved_at: None,
        }
    }

    /// The expiry condition for a queued review is anchor drift, not elapsed time —
    /// so this is the test that stands in for "TTL".
    #[test]
    fn a_comment_is_outdated_only_once_its_anchored_line_changes() {
        let dir = std::env::temp_dir().join(format!("mlr-{}", now_millis()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let path = file.to_string_lossy().to_string();

        assert!(
            !is_outdated(&anchored(&path, 2, Some("two"))),
            "unchanged line must not read as outdated"
        );
        assert!(
            is_outdated(&anchored(&path, 2, Some("TWO"))),
            "a rewritten line must read as outdated"
        );
        assert!(
            is_outdated(&anchored(&path, 99, Some("two"))),
            "a line past the end of the file is outdated"
        );
        // No anchor recorded (older comment) — nothing to compare, so never outdated.
        assert!(!is_outdated(&anchored(&path, 2, None)));

        // The before side is the immutable pre-image; it cannot drift.
        let mut before = anchored(&path, 2, Some("TWO"));
        before.side = DiffSide::Before;
        assert!(!is_outdated(&before));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_deleted_file_makes_its_comments_outdated() {
        let missing = format!("/nonexistent-{}/a.rs", now_millis());
        assert!(is_outdated(&anchored(&missing, 1, Some("one"))));
    }

    /// The review is a file so it has no length limit and survives a delayed
    /// delivery; what gets sent to the agent is only a pointer to it.
    #[test]
    fn writes_the_review_beside_the_repos_moonlight_state() {
        let root = std::env::temp_dir().join(format!("mlroot-{}", now_millis()));
        std::fs::create_dir_all(&root).unwrap();
        let written = write_review_file(
            &root.to_string_lossy(),
            "sess-1",
            "Review of your changes (1):\n",
        )
        .unwrap();

        assert!(written.contains("/.moonlight/reviews/"), "got {written}");
        assert!(written.ends_with("-sess-1.md"), "got {written}");
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            "Review of your changes (1):\n"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn diff_is_empty_when_there_is_no_baseline() {
        let sessions: Arc<dyn ManagedSessionStore> =
            Arc::new(FakeSessions(vec![session("s1", "/repo", true)]));
        let changes: Arc<dyn SessionChangeStore> = Arc::new(FakeChanges {
            touched: HashMap::from([("s1".to_string(), vec![touched("a.txt", false)])]),
            ..Default::default()
        });

        let items = collect_review_queue(&sessions, &changes);
        assert_eq!(items[0].diff, "");
    }
}
