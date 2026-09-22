//! rmcp transport binding: exposes the policy-gated actor verbs as MCP **tools**
//! Claude Code can call. The tool layer is intentionally thin — every call builds an
//! [`ActorRequest`] and delegates to the [`McpActor`] (resolve → PDP → execute →
//! audit → compact); none of the policy logic lives here.
//!
//! Process model (v1): one server **per session** over stdio — the session id is
//! fixed at construction. Claude Code spawns the server and speaks MCP on
//! stdin/stdout. A multi-session in-app HTTP host (reusing the same [`VerbToolServer`]
//! tools) is a later option; it would resolve the session per request instead.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

use moonlight_domain::ids::SessionId;
use moonlight_domain::ports::mcp::{ActorRequest, McpActor};
use moonlight_domain::trust::McpVerb;

/// Parameters for the `run_with_coverage` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunWithCoverageParams {
    /// Optional test target or filter, passed through to the runner (empty = all).
    #[serde(default)]
    pub target: String,
}

/// Parameters for the `run_start` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunStartParams {
    /// The run target to launch — a command id from `run_list_targets` (empty =
    /// the project's default/first target).
    #[serde(default)]
    pub target: String,
}

/// Parameters for the `run_logs` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunLogsParams {
    /// Which run to read — its command from `run_status` (empty = the most
    /// recently started run).
    #[serde(default)]
    pub target: String,
    /// How many tail lines to return (0 = the default 100).
    #[serde(default)]
    pub tail: u32,
}

/// Parameters for the `run_stop` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunStopParams {
    /// Which run to stop — its command from `run_status` (empty = the most
    /// recently started run).
    #[serde(default)]
    pub target: String,
}

/// Parameters for the `request_phase` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RequestPhaseParams {
    /// The workflow phase to move to — one of `plan`, `auto`, `test`, `review`,
    /// `commit`, or `next` to advance one step. Empty also means `next`.
    #[serde(default)]
    pub phase: String,
}

/// Parameters for the `report_blocked` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReportBlockedParams {
    /// A short reason you are blocked (shown to the operator + audited). Optional.
    #[serde(default)]
    pub reason: String,
}

/// Parameters for the `present_plan` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PresentPlanParams {
    /// The plan to present, as Markdown (headings / numbered steps / code fences render
    /// as a document in the IDE's plan-review panel). This is the text the operator reviews.
    pub plan: String,
}

/// An MCP tool server exposing the IDE's actor verbs for **one** Claude Code session,
/// delegating each call to the policy-gated [`McpActor`]. The session is fixed at
/// construction (the per-session stdio process model).
#[derive(Clone)]
pub struct VerbToolServer {
    actor: Arc<dyn McpActor>,
    session: SessionId,
    tool_router: ToolRouter<Self>,
    /// Kept so [`ServerHandler::get_info`] can describe the verbs this host actually
    /// serves. The router already withdraws the rest, but `instructions` is prose the
    /// agent reads as authoritative — naming a tool it does not have sends it looking
    /// for one, which costs a turn and ends in a confused retry.
    scope: VerbScope,
}

/// Which verbs a host offers.
///
/// The workflow verbs are pure engine operations — they send a command, read the
/// phase, or block until an operator decides — so any host can serve them, and the
/// daemon does, which is what lets a session reach them with no IDE attached.
///
/// The `run_*` verbs address "the IDE's Run console", a UI surface a daemon does not
/// have. Offering them from a host that cannot honour them would advertise tools that
/// always fail, so each IDE serves those itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbScope {
    /// Every verb. For a host that owns a Run console.
    All,
    /// Workflow only: `present_plan`, `request_phase`, `phase_status`, `report_blocked`.
    WorkflowOnly,
}

/// What the agent is told this host offers.
///
/// Built from the scope rather than written out once, because the two must agree: a
/// daemon serving [`VerbScope::WorkflowOnly`] used to hand every session a paragraph
/// naming `run_start`, `run_logs` and the rest, none of which it serves.
fn instructions(scope: VerbScope) -> String {
    let mut text = String::from("MoonlightCode actor verbs (policy-gated + audited). Available: ");
    if scope == VerbScope::All {
        text.push_str(
            "run_with_coverage; run_list_targets / run_start / run_stop / run_status / \
             run_logs (the IDE's Run console — shared with the operator); ",
        );
    }
    text.push_str(
        "phase_status (read-only: which workflow phase you are in and what it allows — \
         call it when unsure before requesting a change); request_phase (ask the operator \
         to change the workflow phase — always operator-approved); present_plan (present a \
         plan as Markdown for operator review in the plan panel — blocks on Approve / \
         Refine / Reject; the any-mode, any-backend equivalent of leaving plan mode); \
         report_blocked (signal you are stuck and need the operator — raises a ⚠ on your \
         session).",
    );
    if scope == VerbScope::WorkflowOnly {
        text.push_str(
            " This host has no Run console, so it serves no run_* verbs — use your own \
             shell tooling to build and test.",
        );
    }
    text
}

/// The verbs that need a Run console, and are therefore withdrawn by
/// [`VerbScope::WorkflowOnly`].
const RUN_CONSOLE_VERBS: &[&str] = &[
    "run_with_coverage",
    "run_list_targets",
    "run_start",
    "run_stop",
    "run_status",
    "run_logs",
];

impl VerbToolServer {
    pub fn new(actor: Arc<dyn McpActor>, session: SessionId) -> Self {
        Self::with_scope(actor, session, VerbScope::All)
    }

    /// Build a server offering only the verbs this host can actually honour.
    ///
    /// Routes are *removed* rather than a second router being defined, so both hosts
    /// share one implementation of every verb and cannot drift apart.
    pub fn with_scope(actor: Arc<dyn McpActor>, session: SessionId, scope: VerbScope) -> Self {
        let mut tool_router = Self::tool_router();
        if scope == VerbScope::WorkflowOnly {
            for verb in RUN_CONSOLE_VERBS {
                tool_router.remove_route(verb);
            }
        }
        Self {
            actor,
            session,
            tool_router,
            scope,
        }
    }

    /// Run one verb through the policy-gated actor; the agent always gets text back
    /// (the compact output, or the actor error).
    async fn dispatch(&self, verb: McpVerb, payload: String) -> String {
        let request = ActorRequest {
            session: self.session.clone(),
            verb,
            payload,
        };
        match self.actor.run(&request).await {
            Ok(result) => result.compact_output,
            Err(err) => format!("actor error: {err}"),
        }
    }
}

#[tool_router(router = tool_router)]
impl VerbToolServer {
    /// Run the project's tests with coverage; returns a compact pass/fail summary.
    #[tool(
        name = "run_with_coverage",
        description = "Run the project's tests with coverage and return a compact pass/fail summary."
    )]
    async fn run_with_coverage(&self, params: Parameters<RunWithCoverageParams>) -> String {
        self.dispatch(McpVerb::RunWithCoverage, params.0.target)
            .await
    }

    /// List the project's detected run targets (the IDE Run widget's choices).
    #[tool(
        name = "run_list_targets",
        description = "List the project's detected run targets (commands the IDE Run console can launch)."
    )]
    async fn run_list_targets(&self) -> String {
        self.dispatch(McpVerb::RunListTargets, String::new()).await
    }

    /// Launch a run target in the IDE's Run console (shared with the operator).
    #[tool(
        name = "run_start",
        description = "Launch a run target in the IDE's Run console. `target` = a command id from run_list_targets (empty = default). Replaces the current run."
    )]
    async fn run_start(&self, params: Parameters<RunStartParams>) -> String {
        self.dispatch(McpVerb::RunStart, params.0.target).await
    }

    /// Stop a run (by target command; empty = the most recently started).
    #[tool(
        name = "run_stop",
        description = "Stop a run in the IDE's Run window. `target` = the run's command from run_status (empty = the most recently started run)."
    )]
    async fn run_stop(&self, params: Parameters<RunStopParams>) -> String {
        self.dispatch(McpVerb::RunStop, params.0.target).await
    }

    /// All runs' statuses (one tab per target in the IDE's Run window).
    #[tool(
        name = "run_status",
        description = "The IDE Run window's tabs: each launched target's command and whether it is running / its exit verdict."
    )]
    async fn run_status(&self) -> String {
        self.dispatch(McpVerb::RunStatus, String::new()).await
    }

    /// Tail a run's captured output.
    #[tool(
        name = "run_logs",
        description = "Tail the captured stdout/stderr of a run in the IDE's Run window. `target` = the run's command from run_status (empty = the most recently started run); `tail` = line count (default 100)."
    )]
    async fn run_logs(&self, params: Parameters<RunLogsParams>) -> String {
        // Payload wire shape shared with the executor: `<target>\n<tail>`.
        let payload = format!("{}\n{}", params.0.target, params.0.tail);
        self.dispatch(McpVerb::RunLogs, payload).await
    }

    /// Request a workflow-phase change. Always subject to operator approval — the
    /// agent asks; the human in the cockpit approves or denies.
    #[tool(
        name = "request_phase",
        description = "Request a workflow-phase change (the IDE's Plan → Auto → Test → Review → Commit gate). `phase` = a target phase (`plan`, `auto`, `test`, `review`, `commit`) or `next` (empty = next) to advance one step. The operator must approve every request — use it when you need write access, are done gathering context, or want to move the workflow forward; you cannot change phase on your own."
    )]
    async fn request_phase(&self, params: Parameters<RequestPhaseParams>) -> String {
        self.dispatch(McpVerb::RequestPhase, params.0.phase).await
    }

    /// Self-report that you are blocked and need the operator — raises a ⚠ signal on
    /// your session in the cockpit. Runs in any phase, no approval needed.
    #[tool(
        name = "report_blocked",
        description = "Signal that you are blocked / stuck and need the operator's attention (raises a ⚠ warning on your session in the IDE). `reason` = a short explanation (optional). Use this when you cannot make progress and need a human — it does not stop you, it just calls for help."
    )]
    async fn report_blocked(&self, params: Parameters<ReportBlockedParams>) -> String {
        self.dispatch(McpVerb::ReportBlocked, params.0.reason).await
    }

    /// Present a plan for operator review in the IDE's plan-review panel. Holds for the
    /// operator's Approve / Refine / Reject — the cross-backend, any-mode equivalent of
    /// leaving plan mode.
    #[tool(
        name = "present_plan",
        description = "Present a plan to the operator for review in the IDE's plan-review panel. `plan` = the plan as Markdown. Use this to surface a plan for approval when you are NOT in Claude's native plan mode (e.g. in auto mode, or from another backend) — it opens the same clean plan view and BLOCKS on the operator's decision: Approve (proceed), Refine (revise), or Reject (with feedback). Prefer this over dumping a plan into chat when you want an explicit go/no-go."
    )]
    async fn present_plan(&self, params: Parameters<PresentPlanParams>) -> String {
        self.dispatch(McpVerb::PresentPlan, params.0.plan).await
    }

    /// Report the current workflow phase and what it allows — read-only, no approval.
    #[tool(
        name = "phase_status",
        description = "Report your current workflow phase (Plan / Auto / Test / Review / Commit) and exactly what it allows: whether project-file edits and AI-workspace notes are permitted, and what `next` would advance to. Read-only — no approval, no side effect, works in any phase. Call this first when unsure which phase you are in, before deciding whether to request_phase, so you never act on the wrong phase."
    )]
    async fn phase_status(&self) -> String {
        self.dispatch(McpVerb::PhaseStatus, String::new()).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for VerbToolServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]` — build from default + set fields.
        let mut info = ServerInfo::default();
        info.instructions = Some(instructions(self.scope));
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

/// Serve `server` over stdio (the per-session child-process model: Claude Code spawns
/// this and speaks MCP on stdin/stdout). Resolves when the peer disconnects.
pub async fn serve_stdio(server: VerbToolServer) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::transport::stdio;
    use rmcp::ServiceExt;

    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// Serve `actor`'s verbs for **one** session over an embedded HTTP MCP endpoint at
/// `/mcp` on `listener` (the recommended in-app model). The app binds an ephemeral
/// port per managed session and injects `http://127.0.0.1:<port>/mcp` into that
/// session's `--mcp-config`, so the Claude Code session id is carried by *which*
/// server it talks to — keeping everything in-process, so the live policy/approval/
/// audit adapters all apply. Resolves when the server stops.
pub async fn serve_http(
    actor: Arc<dyn McpActor>,
    session: SessionId,
    listener: tokio::net::TcpListener,
) -> std::io::Result<()> {
    serve_http_scoped(actor, session, listener, VerbScope::All).await
}

/// As [`serve_http`], but offering only the verbs this host can honour.
pub async fn serve_http_scoped(
    actor: Arc<dyn McpActor>,
    session: SessionId,
    listener: tokio::net::TcpListener,
    scope: VerbScope,
) -> std::io::Result<()> {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };

    // A fresh tool server per MCP session, bound to this CC session id.
    let service = StreamableHttpService::new(
        move || {
            Ok(VerbToolServer::with_scope(
                actor.clone(),
                session.clone(),
                scope,
            ))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, router).await
}

/// The app-side host for the embedded MCP server: one shared, session-agnostic
/// [`McpActor`] (the session rides on each request via [`VerbToolServer`]) plus the
/// live [`BusPolicyView`] the composition root feeds from the engine bus. The app
/// calls [`spawn_for`](Self::spawn_for) when it launches a managed session to stand
/// up that session's endpoint and get the URL for its `--mcp-config`.
#[derive(Clone)]
pub struct McpHost {
    actor: Arc<dyn McpActor>,
    policy: Arc<crate::BusPolicyView>,
    scope: VerbScope,
}

impl McpHost {
    pub fn new(actor: Arc<dyn McpActor>, policy: Arc<crate::BusPolicyView>) -> Self {
        Self {
            actor,
            policy,
            scope: VerbScope::All,
        }
    }

    /// The policy-gated actor behind this host, so a caller that already has a session
    /// id can run a verb without going through the wire transport at all.
    ///
    /// This is what lets the control API answer a verb on behalf of a session whose
    /// agent reaches us out-of-process (see the daemon's `mcp` subcommand): the verb
    /// still resolves → PDP → executes → audits exactly once, in the process that holds
    /// the approvals, rather than a second actor being stood up somewhere that cannot
    /// see them.
    pub fn actor(&self) -> Arc<dyn McpActor> {
        self.actor.clone()
    }

    /// A host that offers only the verbs it can honour — the daemon, which has no Run
    /// console, serves workflow verbs only.
    pub fn with_scope(
        actor: Arc<dyn McpActor>,
        policy: Arc<crate::BusPolicyView>,
        scope: VerbScope,
    ) -> Self {
        Self {
            actor,
            policy,
            scope,
        }
    }

    /// The shared policy view — the composition root feeds it the engine bus
    /// (`policy.apply(&event)` per `EngineEvent`).
    pub fn policy(&self) -> Arc<crate::BusPolicyView> {
        self.policy.clone()
    }

    /// Bind an ephemeral loopback port, serve an MCP endpoint for `session` on the
    /// current tokio runtime, and return the URL to put in that session's
    /// `--mcp-config`. **Must be called from within a tokio runtime** (e.g. the
    /// control-server thread), since it binds + spawns.
    pub async fn spawn_for(&self, session: SessionId) -> std::io::Result<String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let actor = self.actor.clone();
        let scope = self.scope;
        tokio::spawn(async move {
            if let Err(err) = serve_http_scoped(actor, session, listener, scope).await {
                tracing::warn!(error = %err, "embedded MCP server stopped");
            }
        });
        Ok(format!("http://127.0.0.1:{port}/mcp"))
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    fn names(scope: VerbScope) -> Vec<String> {
        let mut router = VerbToolServer::tool_router();
        if scope == VerbScope::WorkflowOnly {
            for verb in RUN_CONSOLE_VERBS {
                router.remove_route(verb);
            }
        }
        router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// The prose an agent reads has to match the router. It did not: a workflow-only
    /// host advertised the whole Run console in its instructions while serving none of
    /// it, so a session was told to use tools that were not in its tool list.
    #[test]
    fn the_instructions_name_only_the_verbs_the_host_serves() {
        let workflow = instructions(VerbScope::WorkflowOnly);
        for verb in RUN_CONSOLE_VERBS {
            assert!(
                !workflow.contains(verb),
                "`{verb}` is not served here and must not be advertised"
            );
        }
        for verb in [
            "present_plan",
            "request_phase",
            "phase_status",
            "report_blocked",
        ] {
            assert!(
                workflow.contains(verb),
                "`{verb}` is served and must be named"
            );
        }

        let all = instructions(VerbScope::All);
        for verb in RUN_CONSOLE_VERBS {
            assert!(
                all.contains(verb),
                "a host with a Run console advertises `{verb}`"
            );
        }
    }

    /// The daemon has no Run console. Advertising `run_*` from it would offer an agent
    /// tools that always fail, which is worse than not offering them at all.
    #[test]
    fn a_workflow_only_host_withdraws_the_run_console_verbs() {
        let workflow = names(VerbScope::WorkflowOnly);
        for verb in RUN_CONSOLE_VERBS {
            assert!(
                !workflow.iter().any(|name| name == verb),
                "`{verb}` needs a Run console and must not be offered"
            );
        }
    }

    /// The four that let a session reach the operator must survive the split — without
    /// them a session cannot present a plan or ask to leave its phase.
    #[test]
    fn a_workflow_only_host_keeps_every_workflow_verb() {
        let workflow = names(VerbScope::WorkflowOnly);
        for verb in [
            "present_plan",
            "request_phase",
            "phase_status",
            "report_blocked",
        ] {
            assert!(
                workflow.iter().any(|name| name == verb),
                "`{verb}` is a pure engine operation and must be served everywhere"
            );
        }
    }

    /// An IDE owns a Run console, so it keeps the lot.
    #[test]
    fn an_ide_host_offers_every_verb() {
        assert_eq!(names(VerbScope::All).len(), 10);
    }
}
