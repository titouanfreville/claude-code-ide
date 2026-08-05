//! gRPC panel — a reflection-driven request builder + response viewer (center tab).
//!
//! The sibling of [`http_panel`](super::http_panel), and deliberately shaped like it, but
//! with one step the HTTP tool doesn't need: **Connect**. Protobuf messages aren't
//! self-describing, so the panel first *learns the schema* — from the server's own
//! reflection service, or from `.proto` files named in the active environment — and only
//! then can offer a service/method to call.
//!
//! Once connected the flow is familiar: pick a method, fill in the request, add metadata,
//! Send. Environments are shared with the HTTP tool, so `{{vars}}`, tokens and the
//! `allowed_hosts` host-scope come from one manifest.
//!
//! The request is edited two ways. **Form** is the default and the point of the whole
//! exercise: reflection already told us every field and its type, so the panel renders
//! that as a table of cells and the operator picks values instead of retyping the shape
//! from memory. Every cell defaults to `null` — unset, and therefore not sent at all.
//! **JSON** is the escape hatch for what a table can't say, and the two convert into each
//! other on switch so neither is a dead end.
//!
//! Unary methods only in this increment. Streaming methods are listed but disabled, with
//! the reason shown, rather than hidden — a method missing from the list would read as a
//! discovery bug.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    MouseDownEvent, SharedString, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, TabPanel};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable;

use super::CloseTab;
use crate::grpc::{self, GrpcCall, GrpcOutcome, MethodInfo, SchemaSource};
use crate::http;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// Cap the response body actually rendered (the stored body is already ≤ 200 KB).
const RENDER_BODY: usize = 50_000;
/// Above this many lines the response is drawn as one block instead of click-to-copy
/// rows — past it the element count costs more than the affordance is worth, and Copy
/// still takes the whole body in one go.
const COPY_LINES: usize = 400;
/// How long the toolbar admits to having copied something.
const COPY_FLASH: std::time::Duration = std::time::Duration::from_millis(1400);

/// Which request pane is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReqTab {
    Message,
    Metadata,
    Schema,
}

/// Which response pane is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RespTab {
    Response,
    Trailers,
}

/// How the request message is being edited.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyMode {
    /// One row per field, built from the descriptor.
    Form,
    /// Raw protobuf-JSON.
    Json,
}

/// One editable metadata row. Each cell is its own `InputState`; `on` is the enable box.
struct KvRow {
    key: Entity<InputState>,
    value: Entity<InputState>,
    on: bool,
}

/// One row of the request form: the descriptor's field, plus whatever is in its cell.
struct FormRow {
    field: grpc::FormField,
    /// Text and JSON cells own an input; bool, enum and group cells don't.
    input: Option<Entity<InputState>>,
    /// The current pick for a bool or enum cell. `None` is the unset (`null`) state.
    picked: Option<String>,
    /// Group rows only: whether the fields beneath are hidden.
    collapsed: bool,
}

impl FormRow {
    /// The values this row's cell cycles through, unset first.
    fn choices(&self) -> Vec<String> {
        match &self.field.input {
            grpc::FormInput::Bool => vec!["true".to_string(), "false".to_string()],
            grpc::FormInput::Enum(values) => values.clone(),
            _ => Vec::new(),
        }
    }

    /// Advance the cell: unset → first → … → last → unset. A cycle rather than a popup
    /// because these lists are short and a popup layer would cover the rows below it.
    fn cycle(&mut self) {
        let choices = self.choices();
        if choices.is_empty() {
            return;
        }
        let next = match &self.picked {
            None => Some(choices[0].clone()),
            Some(current) => match choices.iter().position(|c| c == current) {
                Some(i) if i + 1 < choices.len() => Some(choices[i + 1].clone()),
                _ => None,
            },
        };
        self.picked = next;
    }
}

pub struct GrpcPanel {
    /// Inputs are stood up lazily (they need a `Window`).
    target_input: Option<Entity<InputState>>,
    message_input: Option<Entity<InputState>>,
    metadata_rows: Vec<KvRow>,

    /// The discovered schema, and where it came from.
    schema: Option<grpc::Schema>,
    schema_source: Option<SchemaSource>,
    services: Vec<String>,
    service: Option<String>,
    methods: Vec<MethodInfo>,
    method: Option<String>,
    /// Toggled pick-lists (a list panel, matching the saved-requests list, rather than a
    /// native dropdown — same interaction the rest of this panel uses).
    service_open: bool,
    method_open: bool,
    connecting: bool,

    req_tab: ReqTab,
    body_mode: BodyMode,
    form_rows: Vec<FormRow>,
    /// Set when the selected method changes. The rows are rebuilt on the next render,
    /// which is where the `Window` an input needs is available.
    form_dirty: bool,
    /// JSON to seed the rebuilt form from — a saved call, the example skeleton, or the
    /// JSON tab's text.
    form_seed: Option<String>,
    /// Why a Form/JSON switch was refused, shown under the tab bar.
    form_note: Option<String>,

    response: Option<GrpcOutcome>,
    resp_tab: RespTab,
    loading: bool,
    /// What was last put on the clipboard, shown briefly so a click that produces no
    /// visible change still confirms itself.
    copied: Option<SharedString>,
    /// A pre-send refusal (no method picked, host not allowed, bad target) shown in place
    /// of a response.
    error: Option<grpc::GrpcError>,

    /// Selected environment name; `None` = the manifest's active/default env.
    env: Option<String>,
    saved: Vec<grpc::SavedGrpcRequest>,
    saved_open: bool,
    needs_init: bool,

    focus_handle: FocusHandle,
    tab_panel: Option<WeakEntity<TabPanel>>,
    tab_menu: Option<super::TabMenu>,
}

impl GrpcPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            target_input: None,
            message_input: None,
            metadata_rows: Vec::new(),
            schema: None,
            schema_source: None,
            services: Vec::new(),
            service: None,
            methods: Vec::new(),
            method: None,
            service_open: false,
            method_open: false,
            connecting: false,
            req_tab: ReqTab::Message,
            body_mode: BodyMode::Form,
            form_rows: Vec::new(),
            form_dirty: false,
            form_seed: None,
            form_note: None,
            response: None,
            resp_tab: RespTab::Response,
            loading: false,
            copied: None,
            error: None,
            env: None,
            saved: Vec::new(),
            saved_open: false,
            needs_init: true,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
        }
    }

    /// Layout-restore is a scratch builder — start fresh (a schema is a live connection's
    /// state, not something to rehydrate from disk).
    pub fn restore(_info: &PanelInfo, cx: &mut Context<Self>) -> Self {
        Self::new(cx)
    }

    /// One gRPC tab (dedup key shared with `OpenRequest::Grpc`).
    pub fn tab_key() -> &'static str {
        "grpc"
    }

    fn make_row(k: &str, v: &str, window: &mut Window, cx: &mut Context<Self>) -> KvRow {
        let key = cx.new(|cx| InputState::new(window, cx).placeholder("metadata-key"));
        let value = cx.new(|cx| InputState::new(window, cx).placeholder("value"));
        if !k.is_empty() {
            key.update(cx, |s, cx| s.set_value(k, window, cx));
        }
        if !v.is_empty() {
            value.update(cx, |s, cx| s.set_value(v, window, cx));
        }
        KvRow {
            key,
            value,
            on: true,
        }
    }

    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = Self::make_row("", "", window, cx);
        self.metadata_rows.push(row);
        cx.notify();
    }

    /// Collect enabled, non-empty metadata rows, interpolating values.
    fn collect_metadata(
        &self,
        vars: &std::collections::BTreeMap<String, String>,
        cx: &App,
    ) -> Vec<(String, String)> {
        self.metadata_rows
            .iter()
            .filter(|r| r.on)
            .filter_map(|r| {
                let k = r.key.read(cx).value().trim().to_string();
                if k.is_empty() {
                    return None;
                }
                let raw = r.value.read(cx).value();
                Some((k, grpc::interpolate(&raw, vars)))
            })
            .collect()
    }

    fn project_root(cx: &App) -> Option<PathBuf> {
        cx.try_global::<ShellDeps>()
            .map(|d| d.focus.read(cx).root())
    }

    fn manifest(cx: &App) -> http::HttpManifest {
        Self::project_root(cx)
            .map(|root| http::load_manifest(&root))
            .unwrap_or_default()
    }

    /// The environment this panel is using — the same manifest the HTTP tool reads.
    fn active_env(&self, cx: &App) -> http::HttpEnv {
        let manifest = Self::manifest(cx);
        match &self.env {
            Some(name) => manifest
                .environments
                .get(name)
                .cloned()
                .unwrap_or_else(|| manifest.active_env()),
            None => manifest.active_env(),
        }
    }

    fn env_label(&self, cx: &App) -> String {
        self.env
            .clone()
            .or_else(|| Self::manifest(cx).active_name())
            .unwrap_or_else(|| "local only".into())
    }

    fn input_text(input: &Option<Entity<InputState>>, cx: &App) -> String {
        input
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// The method currently selected, if any.
    fn selected_method(&self) -> Option<&MethodInfo> {
        let name = self.method.as_ref()?;
        self.methods.iter().find(|m| &m.name == name)
    }

    /// The request message descriptor for the current service/method.
    fn request_type(&self) -> Option<prost_reflect::MessageDescriptor> {
        let (schema, service, method) = (
            self.schema.as_ref()?,
            self.service.as_ref()?,
            self.method.as_ref()?,
        );
        schema.method_types(service, method).map(|(req, _)| req)
    }

    /// Queue a form rebuild, optionally seeding it from a JSON message. Rebuilding needs
    /// a `Window`, which the async paths (Connect) don't have — so every caller marks and
    /// `render` does the work.
    fn mark_form_dirty(&mut self, seed: Option<String>) {
        self.form_dirty = true;
        self.form_seed = seed;
        self.form_note = None;
    }

    /// Rebuild the form's rows from the selected method's request descriptor, carrying
    /// over `form_seed` if one was queued.
    fn rebuild_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form_dirty = false;
        let seed = self.form_seed.take();
        let fields = match (&self.schema, self.request_type()) {
            (Some(schema), Some(req)) => schema.form_fields(&req),
            _ => Vec::new(),
        };
        let values = seed
            .map(|json| grpc::seed_form_values(&fields, &json))
            .unwrap_or_default();

        self.form_rows = fields
            .into_iter()
            .map(|field| {
                let seeded = values.get(&field.path).cloned();
                let (input, picked) = match &field.input {
                    // Bool and enum cells are click-cycled, so they hold their value
                    // directly rather than owning an input.
                    grpc::FormInput::Bool | grpc::FormInput::Enum(_) => (None, seeded),
                    grpc::FormInput::Group => (None, None),
                    grpc::FormInput::Text { .. } | grpc::FormInput::Json { .. } => {
                        let placeholder = match &field.input {
                            grpc::FormInput::Json { hint } => hint.clone(),
                            _ => "null".to_string(),
                        };
                        let state =
                            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                        if let Some(value) = &seeded {
                            state.update(cx, |s, cx| s.set_value(value, window, cx));
                        }
                        (Some(state), None)
                    }
                };
                FormRow {
                    field,
                    input,
                    picked,
                    collapsed: false,
                }
            })
            .collect();
        cx.notify();
    }

    /// Read every cell back out, keyed by field path.
    fn form_values(&self, cx: &App) -> std::collections::BTreeMap<String, String> {
        self.form_rows
            .iter()
            .filter_map(|row| {
                let value = match (&row.input, &row.picked) {
                    (Some(input), _) => input.read(cx).value().to_string(),
                    (None, Some(picked)) => picked.clone(),
                    _ => return None,
                };
                Some((row.field.path.clone(), value))
            })
            .collect()
    }

    /// The form's cells as a protobuf-JSON message.
    fn form_json(&self, cx: &App) -> String {
        let fields: Vec<grpc::FormField> = self.form_rows.iter().map(|r| r.field.clone()).collect();
        grpc::build_message_json(&fields, &self.form_values(cx))
    }

    /// The request message, whichever way it is currently being authored.
    fn request_json(&self, cx: &App) -> String {
        match self.body_mode {
            BodyMode::Form => self.form_json(cx),
            BodyMode::Json => Self::input_text(&self.message_input, cx),
        }
    }

    /// Switch how the message is edited, carrying the current message across.
    ///
    /// Form → JSON always works. JSON → Form only works if the text parses, because
    /// seeding from unparseable JSON would silently blank the table — so an invalid
    /// message keeps the operator in the tab where they can see and fix it.
    fn set_body_mode(&mut self, mode: BodyMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == self.body_mode {
            return;
        }
        match mode {
            BodyMode::Json => {
                let json = self.form_json(cx);
                if let Some(input) = &self.message_input {
                    input.update(cx, |s, cx| s.set_value(json, window, cx));
                }
                self.body_mode = BodyMode::Json;
                self.form_note = None;
            }
            BodyMode::Form => {
                let text = Self::input_text(&self.message_input, cx);
                let trimmed = text.trim();
                if !trimmed.is_empty()
                    && serde_json::from_str::<serde_json::Value>(trimmed).is_err()
                {
                    self.form_note = Some(
                        "this JSON doesn't parse, so the form can't read it — fix it here \
                         first, or clear it to start from the form"
                            .into(),
                    );
                    cx.notify();
                    return;
                }
                self.body_mode = BodyMode::Form;
                self.mark_form_dirty(Some(text));
                self.rebuild_form(window, cx);
            }
        }
        cx.notify();
    }

    /// Resolve + host-scope the typed target. Shared by Connect and Send so both refuse
    /// the same things for the same reasons.
    fn resolve_target(&self, cx: &App) -> Result<grpc::GrpcTarget, grpc::GrpcError> {
        let env = self.active_env(cx);
        let raw = grpc::interpolate(&Self::input_text(&self.target_input, cx), &env.vars);
        let target = grpc::parse_target(&raw).map_err(|e| {
            grpc::GrpcError::new("Not a usable gRPC target", e).with_hint(
                "a gRPC target is `host:port` — optionally with a grpc:// or grpcs:// scheme",
            )
        })?;
        if !grpc::target_allowed(&target, &env.allowed_hosts) {
            return Err(grpc::GrpcError::new(
                format!("{} is not an allowed host", target.authority),
                format!(
                    "the `{}` environment allows only loopback, private addresses, and \
                     its own allowed_hosts list",
                    self.env_label(cx)
                ),
            )
            .with_hint("add the host to `allowed_hosts` in .moonlight/http/environments.json"));
        }
        Ok(target)
    }

    /// Discover the schema: reflection, then the environment's `.proto` files.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let target = match self.resolve_target(cx) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let Some(rt) = cx.try_global::<ShellDeps>().and_then(|d| d.grpc_rt.clone()) else {
            self.error = Some(runtime_missing());
            cx.notify();
            return;
        };
        let env = self.active_env(cx);

        self.connecting = true;
        self.error = None;
        cx.notify();

        // Runs on the tokio reactor; the JoinHandle is awaited on GPUI's executor, so
        // neither the UI thread nor the reactor is blocked.
        let task = rt.spawn(async move { grpc::load_schema(&target, &env).await });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.connecting = false;
                match result {
                    Ok(Ok((schema, source))) => {
                        this.services = schema.services();
                        // Pre-select when there is no ambiguity, so the common
                        // single-service case is one click, not three.
                        this.service = this.services.first().cloned();
                        this.methods = this
                            .service
                            .as_ref()
                            .map(|s| schema.methods(s))
                            .unwrap_or_default();
                        this.method = this
                            .methods
                            .iter()
                            .find(|m| m.is_unary())
                            .map(|m| m.name.clone());
                        this.schema = Some(schema);
                        this.schema_source = Some(source);
                        this.error = None;
                        // The method changed, so the form's fields did. Carry over
                        // whatever message is already in hand — filled into the form,
                        // typed in the JSON tab, or restored by a saved call that had no
                        // schema to bind to yet — so reconnecting never costs the work.
                        let held = this.request_json(cx);
                        this.mark_form_dirty(
                            Some(held).filter(|h| !h.trim().is_empty() && h.trim() != "{}"),
                        );
                    }
                    Ok(Err(e)) => {
                        this.schema = None;
                        this.schema_source = None;
                        this.services.clear();
                        this.methods.clear();
                        this.form_rows.clear();
                        this.error = Some(e);
                    }
                    Err(e) => {
                        this.error = Some(grpc::GrpcError::new(
                            "The connect task did not finish",
                            e.to_string(),
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_service(&mut self, name: String, cx: &mut Context<Self>) {
        self.methods = self
            .schema
            .as_ref()
            .map(|s| s.methods(&name))
            .unwrap_or_default();
        self.method = self
            .methods
            .iter()
            .find(|m| m.is_unary())
            .map(|m| m.name.clone());
        self.service = Some(name);
        self.service_open = false;
        self.response = None;
        self.mark_form_dirty(None);
        cx.notify();
    }

    /// Seed the request with a skeleton of its type — every field present, holding a zero
    /// value of the right shape.
    fn fill_example(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(schema), Some(req)) = (&self.schema, self.request_type()) else {
            return;
        };
        let example = schema.example_json(&req);
        match self.body_mode {
            BodyMode::Form => {
                self.mark_form_dirty(Some(example));
                self.rebuild_form(window, cx);
            }
            BodyMode::Json => {
                if let Some(i) = &self.message_input {
                    i.update(cx, |s, cx| s.set_value(example, window, cx));
                }
            }
        }
        self.req_tab = ReqTab::Message;
        cx.notify();
    }

    /// Build → host-scope → call (off the UI thread) → record in history.
    fn send(&mut self, cx: &mut Context<Self>) {
        let (Some(schema), Some(service)) = (self.schema.clone(), self.service.clone()) else {
            self.error = Some(
                grpc::GrpcError::new(
                    "No schema loaded",
                    "the panel doesn't know this server's messages yet",
                )
                .with_hint("enter a target and press Connect"),
            );
            cx.notify();
            return;
        };
        let Some(method) = self.selected_method().cloned() else {
            self.error = Some(grpc::GrpcError::new(
                "No method selected",
                format!("{service} is connected, but no method is chosen"),
            ));
            cx.notify();
            return;
        };
        if let Some(kind) = method.streaming {
            self.error = Some(
                grpc::GrpcError::new(
                    format!("{} is a {kind} method", method.name),
                    "this panel makes unary calls only",
                )
                .with_hint("pick a unary method, or use grpcurl for the streaming ones"),
            );
            cx.notify();
            return;
        }
        let Some((req_desc, resp_desc)) = schema.method_types(&service, &method.name) else {
            self.error = Some(
                grpc::GrpcError::new(
                    format!("{service}/{} is not in the loaded schema", method.name),
                    "the selection is from an older connection",
                )
                .with_hint("press Connect to reload the schema"),
            );
            cx.notify();
            return;
        };
        let target = match self.resolve_target(cx) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(e);
                self.response = None;
                cx.notify();
                return;
            }
        };
        let Some(rt) = cx.try_global::<ShellDeps>().and_then(|d| d.grpc_rt.clone()) else {
            self.error = Some(runtime_missing());
            cx.notify();
            return;
        };

        let env = self.active_env(cx);
        let metadata = self.collect_metadata(&env.vars, cx);
        let raw_message = self.request_json(cx);
        // An empty editor means the empty message, which is a legitimate request.
        let message = if raw_message.trim().is_empty() {
            "{}".to_string()
        } else {
            grpc::interpolate(&raw_message, &env.vars)
        };

        let spec = grpc::CallSpec {
            target: target.clone(),
            path: method.path.clone(),
            request: req_desc,
            response: resp_desc,
            metadata,
            message,
        };
        let label = format!("{service}/{}", method.name);
        let authority = target.authority.clone();
        let history = cx.try_global::<ShellDeps>().map(|d| d.grpc_history.clone());

        self.loading = true;
        self.error = None;
        self.response = None;
        cx.notify();

        let task = rt.spawn(async move { grpc::call(spec).await });
        cx.spawn(async move |weak, cx| {
            let outcome = match task.await {
                Ok(o) => o,
                Err(e) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.loading = false;
                        this.error = Some(grpc::GrpcError::new(
                            "The call task did not finish",
                            e.to_string(),
                        ));
                        cx.notify();
                    });
                    return;
                }
            };
            if let Some(h) = history {
                h.record(GrpcCall {
                    method: label,
                    target: authority,
                    code: outcome.code.clone(),
                    ms: outcome.ms,
                    ok: outcome.ok,
                    bytes: outcome.bytes,
                    at_millis: now_millis(),
                    // The history list has one line per call, so the three-part error
                    // collapses to its summary there.
                    error: outcome.error.as_ref().map(|e| e.one_line()),
                });
            }
            let _ = weak.update(cx, |this, cx| {
                this.loading = false;
                this.resp_tab = RespTab::Response;
                this.response = Some(outcome);
                cx.notify();
            });
        })
        .detach();
    }

    /// Put `text` on the clipboard and say so.
    ///
    /// GPUI's text isn't selectable with a mouse, so without an explicit copy a response
    /// can be read but not taken — and taking it is the whole reason to look at it.
    fn copy(&mut self, text: String, label: impl Into<SharedString>, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(label.into());
        cx.notify();
        // Clear the flash, so the toolbar never claims a copy that happened minutes ago.
        cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(COPY_FLASH).await;
            let _ = weak.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// The whole of whichever response pane is showing, and what to call it.
    fn response_text(&self, resp: &GrpcOutcome) -> (String, &'static str) {
        match self.resp_tab {
            RespTab::Trailers => (
                resp.metadata
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                "trailers",
            ),
            RespTab::Response => match (&resp.error, resp.ok) {
                (Some(err), _) => (err.as_block(), "error"),
                (None, false) => {
                    let code = resp.code.clone().unwrap_or_default();
                    let message = if resp.status_message.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", resp.status_message)
                    };
                    (format!("{code}{message}"), "status")
                }
                (None, true) => (resp.body.clone(), "response"),
            },
        }
    }

    /// Save the current call to `requests.json` (upsert by `Service/Method` name).
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(root) = Self::project_root(cx) else {
            return;
        };
        let (Some(service), Some(method)) = (self.service.clone(), self.method.clone()) else {
            return;
        };
        // Save the authored text, not the interpolated result, so `{{vars}}` survive.
        let no_vars = std::collections::BTreeMap::new();
        let req = grpc::SavedGrpcRequest {
            name: clip(&format!("{service}/{method}"), 60),
            target: Self::input_text(&self.target_input, cx),
            service,
            method,
            metadata: self.collect_metadata(&no_vars, cx),
            message: self.request_json(cx),
        };
        let mut store = grpc::load_requests(&root);
        match store.requests.iter_mut().find(|r| r.name == req.name) {
            Some(existing) => *existing = req,
            None => store.requests.push(req),
        }
        let _ = grpc::save_requests(&root, &store);
        self.saved = store.requests;
        cx.notify();
    }

    /// Load a saved call into the editors. The service/method are restored as names; they
    /// bind to real descriptors on the next Connect.
    fn load_saved(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(req) = self.saved.get(idx).cloned() else {
            return;
        };
        if let Some(i) = &self.target_input {
            i.update(cx, |s, cx| s.set_value(req.target.clone(), window, cx));
        }
        if let Some(i) = &self.message_input {
            i.update(cx, |s, cx| s.set_value(req.message.clone(), window, cx));
        }
        self.metadata_rows = req
            .metadata
            .iter()
            .map(|(k, v)| Self::make_row(k, v, window, cx))
            .collect();
        self.service = Some(req.service.clone());
        self.method = Some(req.method.clone());
        // Re-bind the method list if a schema is already loaded for this service.
        if let Some(schema) = &self.schema {
            self.methods = schema.methods(&req.service);
        }
        // The saved message seeds the form; with no schema loaded there are no rows to
        // seed yet, and the next Connect picks it up.
        self.mark_form_dirty(Some(req.message.clone()));
        self.rebuild_form(window, cx);
        self.saved_open = false;
        cx.notify();
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

impl Focusable for GrpcPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl super::TabMenuHost for GrpcPanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for GrpcPanel {}

impl Panel for GrpcPanel {
    fn panel_name(&self) -> &'static str {
        "Grpc"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::tab_title(
            "gRPC".to_string(),
            None,
            cx.entity_id().as_u64(),
            self.tab_panel.clone(),
            Arc::new(cx.entity()),
            self.focus_handle(cx),
            cx,
            |menu| menu,
        )
    }

    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
    }

    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::panel(serde_json::json!({ "key": Self::tab_key() }));
        state
    }
}

/// A small toolbar pill button.
fn pill(
    id: &'static str,
    label: String,
    accent: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (fg, border) = if accent {
        (theme::accent(), theme::accent())
    } else {
        (theme::text_secondary(), theme::border_subtle())
    };
    div()
        .id(id)
        .flex_none()
        .px(px(8.))
        .py(px(3.))
        .rounded(theme::radius_sm())
        .border_1()
        .border_color(border)
        .text_size(theme::text_xs())
        .text_color(fg)
        .cursor_pointer()
        .hover(|d| d.bg(theme::row_hover()).text_color(theme::text_primary()))
        .on_click(on_click)
        .child(label)
}

impl Render for GrpcPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.target_input.is_none() {
            self.target_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder("localhost:50051  or  {{grpc_host}}")
            }));
            self.message_input = Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .auto_grow(4, 16)
                    .placeholder("request message as JSON — {{vars}} ok")
            }));
            self.metadata_rows.push(Self::make_row("", "", window, cx));
        }
        if self.needs_init {
            self.needs_init = false;
            if let Some(root) = Self::project_root(cx) {
                self.saved = grpc::load_requests(&root).requests;
            }
        }
        // Rebuilding needs the `Window` that only render has, so the async paths queue it.
        if self.form_dirty {
            self.rebuild_form(window, cx);
        }

        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        let env_label = self.env_label(cx);
        let saved_count = self.saved.len();

        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                super::close_this_tab(&this.tab_panel, cx.entity(), window, cx)
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_base())
            .text_color(theme::text_primary())
            // ── Target line: target · Connect · env · saved · save ──────────────────
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .px_2()
                    .py(px(5.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .children(self.target_input.as_ref().map(Input::new)),
                    )
                    .when(self.connecting, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(theme::text_xs())
                                .text_color(theme::text_muted())
                                .child("connecting…"),
                        )
                    })
                    .child(pill(
                        "grpc-connect",
                        "Connect".into(),
                        self.schema.is_none(),
                        cx.listener(|this, _e, _w, cx| this.connect(cx)),
                    ))
                    .child(pill(
                        "grpc-env",
                        format!("env: {env_label}"),
                        false,
                        cx.listener(|_this, _e, _w, _cx| {}),
                    ))
                    .child(pill(
                        "grpc-saved",
                        format!("saved ({saved_count}) ▾"),
                        false,
                        cx.listener(|this, _e, _w, cx| {
                            this.saved_open = !this.saved_open;
                            cx.notify();
                        }),
                    ))
                    .child(pill(
                        "grpc-save",
                        "＋ Save".into(),
                        false,
                        cx.listener(|this, _e, _w, cx| this.save(cx)),
                    )),
            )
            .when(self.saved_open, |d| d.child(self.saved_list(cx)))
            // ── Service / method pickers ────────────────────────────────────────────
            .child(self.method_bar(cx))
            .when(self.service_open, |d| d.child(self.service_list(cx)))
            .when(self.method_open, |d| d.child(self.method_list(cx)))
            // ── Request config (Message / Metadata / Schema) ────────────────────────
            .child(self.request_config(cx))
            // ── Response ────────────────────────────────────────────────────────────
            .child(self.response_region(cx))
            .children(super::tab_menu_overlay(
                self.tab_menu.as_ref(),
                dismiss,
                window,
            ))
    }
}

impl GrpcPanel {
    /// The service + method pickers and the Send button.
    fn method_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.schema.is_some();
        let service = self
            .service
            .clone()
            .unwrap_or_else(|| "(no service)".into());
        let method = self.method.clone().unwrap_or_else(|| "(no method)".into());
        let source = self
            .schema_source
            .map(|s| s.label().to_string())
            .unwrap_or_default();

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .w_full()
            .px_2()
            .py(px(4.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .when(!connected, |d| {
                d.child(
                    div()
                        .flex_1()
                        .text_size(theme::text_xs())
                        .text_color(theme::text_muted())
                        .child("not connected — enter a target and press Connect"),
                )
            })
            .when(connected, |d| {
                d.child(pill(
                    "grpc-service",
                    format!("{service} ▾"),
                    false,
                    cx.listener(|this, _e, _w, cx| {
                        this.service_open = !this.service_open;
                        this.method_open = false;
                        cx.notify();
                    }),
                ))
                .child(pill(
                    "grpc-method",
                    format!("{method} ▾"),
                    false,
                    cx.listener(|this, _e, _w, cx| {
                        this.method_open = !this.method_open;
                        this.service_open = false;
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(theme::text_2xs())
                        .text_color(theme::text_muted())
                        .child(format!("via {source}")),
                )
                .when(self.loading, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_size(theme::text_xs())
                            .text_color(theme::text_muted())
                            .child("calling…"),
                    )
                })
                .child(pill(
                    "grpc-send",
                    "Send ▸".into(),
                    true,
                    cx.listener(|this, _e, _w, cx| this.send(cx)),
                ))
            })
    }

    fn service_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .id("grpc-service-list")
            .flex()
            .flex_col()
            .max_h(px(160.))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_sunken());
        for (i, name) in self.services.iter().enumerate() {
            let name = name.clone();
            let picked = self.service.as_deref() == Some(name.as_str());
            col = col.child(
                div()
                    .id(("grpc-svc-row", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_3()
                    .py(px(3.))
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::row_hover()))
                    .text_size(theme::text_xs())
                    .text_color(if picked {
                        theme::text_primary()
                    } else {
                        theme::text_secondary()
                    })
                    .child(name.clone())
                    .on_click(
                        cx.listener(move |this, _e, _w, cx| this.select_service(name.clone(), cx)),
                    ),
            );
        }
        col
    }

    /// The method list. Streaming methods are shown but not selectable, with the reason
    /// beside them — hiding them would read as a discovery failure.
    fn method_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .id("grpc-method-list")
            .flex()
            .flex_col()
            .max_h(px(200.))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_sunken());
        for (i, m) in self.methods.iter().enumerate() {
            let name = m.name.clone();
            let unary = m.is_unary();
            let note = m.streaming.unwrap_or("");
            let picked = self.method.as_deref() == Some(name.as_str());
            let mut row = div()
                .id(("grpc-m-row", i))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py(px(3.))
                .text_size(theme::text_xs())
                .text_color(if !unary {
                    theme::text_muted()
                } else if picked {
                    theme::text_primary()
                } else {
                    theme::text_secondary()
                })
                .child(name.clone());
            if !unary {
                row = row.child(
                    div()
                        .flex_none()
                        .text_size(theme::text_2xs())
                        .text_color(theme::text_muted())
                        .child(format!("· {note} — not supported yet")),
                );
            } else {
                row = row
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::row_hover()))
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        this.method = Some(name.clone());
                        this.method_open = false;
                        this.response = None;
                        // A different method is a different message shape.
                        this.mark_form_dirty(None);
                        cx.notify();
                    }));
            }
            col = col.child(row);
        }
        col
    }

    fn saved_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .id("grpc-saved-list")
            .flex()
            .flex_col()
            .max_h(px(160.))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_sunken());
        if self.saved.is_empty() {
            return col.child(
                div()
                    .px_3()
                    .py_1()
                    .text_size(theme::text_2xs())
                    .text_color(theme::text_muted())
                    .child("no saved calls — build one and ＋ Save"),
            );
        }
        for (i, r) in self.saved.iter().enumerate() {
            let name = r.name.clone();
            col = col.child(
                div()
                    .id(("grpc-saved-row", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_3()
                    .py(px(3.))
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::row_hover()))
                    .text_size(theme::text_xs())
                    .text_color(theme::text_secondary())
                    .child(name)
                    .on_click(
                        cx.listener(move |this, _e, window, cx| this.load_saved(i, window, cx)),
                    ),
            );
        }
        col
    }

    /// Message / Metadata / Schema in one scrollable pane (capped height).
    fn request_config(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("grpc-config")
            .flex_none()
            .max_h(px(300.))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(theme::border_subtle())
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pt(px(4.))
                    .pb(px(3.))
                    .child(tab_chip(
                        "grpc-tab-msg",
                        "Message",
                        self.req_tab == ReqTab::Message,
                        cx.listener(|this, _e, _w, cx| {
                            this.req_tab = ReqTab::Message;
                            cx.notify();
                        }),
                    ))
                    .child(tab_chip(
                        "grpc-tab-meta",
                        &format!("Metadata ({})", self.metadata_rows.len()),
                        self.req_tab == ReqTab::Metadata,
                        cx.listener(|this, _e, _w, cx| {
                            this.req_tab = ReqTab::Metadata;
                            cx.notify();
                        }),
                    ))
                    .child(tab_chip(
                        "grpc-tab-schema",
                        "Schema",
                        self.req_tab == ReqTab::Schema,
                        cx.listener(|this, _e, _w, cx| {
                            this.req_tab = ReqTab::Schema;
                            cx.notify();
                        }),
                    ))
                    .child(div().flex_1().min_w(px(0.)))
                    // Form/JSON and Fill example only mean anything once the panel knows
                    // the message shape.
                    .when(
                        self.req_tab == ReqTab::Message && self.selected_method().is_some(),
                        |d| {
                            d.child(mode_chip(
                                "grpc-mode-form",
                                "Form",
                                self.body_mode == BodyMode::Form,
                                cx.listener(|this, _e, window, cx| {
                                    this.set_body_mode(BodyMode::Form, window, cx)
                                }),
                            ))
                            .child(mode_chip(
                                "grpc-mode-json",
                                "JSON",
                                self.body_mode == BodyMode::Json,
                                cx.listener(|this, _e, window, cx| {
                                    this.set_body_mode(BodyMode::Json, window, cx)
                                }),
                            ))
                            .child(pill(
                                "grpc-example",
                                "＋ Fill example".into(),
                                false,
                                cx.listener(|this, _e, window, cx| this.fill_example(window, cx)),
                            ))
                        },
                    ),
            )
            .children(self.form_note.clone().map(|note| {
                div()
                    .px_2()
                    .pb(px(3.))
                    .text_size(theme::text_2xs())
                    .text_color(theme::status_color(
                        moonlight_domain::session::SessionStatus::Errored,
                    ))
                    .child(note)
            }))
            .child(match self.req_tab {
                ReqTab::Message => match self.body_mode {
                    BodyMode::Form => self.form_table(cx).into_any_element(),
                    BodyMode::Json => div()
                        .px_2()
                        .pb(px(4.))
                        .children(self.message_input.as_ref().map(Input::new))
                        .into_any_element(),
                },
                ReqTab::Metadata => self.metadata_table(cx).into_any_element(),
                ReqTab::Schema => self.schema_view().into_any_element(),
            })
    }

    /// The request message as a table: one row per field, `name · type · value`.
    ///
    /// The type column is the reason this beats a JSON box — it is exactly what
    /// reflection knows and a text editor can't tell you. Every cell starts at `null`,
    /// so an untouched form reads as a column of `null`s: the honest picture of a message
    /// where nothing has been set.
    fn form_table(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.selected_method().is_none() {
            return form_placeholder("pick a method to see the fields it takes");
        }
        if self.form_rows.is_empty() {
            let name = self
                .request_type()
                .map(|d| d.full_name().to_string())
                .unwrap_or_else(|| "this request".into());
            return form_placeholder(format!("{name} has no fields — press Send ▸"));
        }

        // A collapsed group hides everything pathed beneath it.
        let hidden: Vec<String> = self
            .form_rows
            .iter()
            .filter(|r| r.collapsed)
            .map(|r| format!("{}.", r.field.path))
            .collect();

        let mut col = div().flex().flex_col().gap(px(2.)).px_2().pb(px(6.)).child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .pb(px(2.))
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child(div().w(px(140.)).flex_none().child("field"))
                .child(div().w(px(110.)).flex_none().child("type"))
                .child(div().flex_1().child("value")),
        );

        for (i, row) in self.form_rows.iter().enumerate() {
            if hidden.iter().any(|p| row.field.path.starts_with(p)) {
                continue;
            }
            col = col.child(self.form_row(i, row, cx));
        }
        col.into_any_element()
    }

    fn form_row(&self, i: usize, row: &FormRow, cx: &mut Context<Self>) -> AnyElement {
        let indent = px(10. * row.field.depth as f32);
        let is_group = matches!(row.field.input, grpc::FormInput::Group);

        let name_cell = div()
            .flex_none()
            .w(px(140.))
            .pl(indent)
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .font_family(theme::mono_font())
            .text_size(theme::text_xs())
            .text_color(if is_group {
                theme::text_muted()
            } else {
                theme::text_secondary()
            })
            .when(is_group, |d| {
                d.child(if row.collapsed { "›" } else { "⌄" })
            })
            .child(row.field.name.clone());

        let type_cell = div()
            .flex_none()
            .w(px(110.))
            .font_family(theme::mono_font())
            .text_size(theme::text_2xs())
            .text_color(theme::text_muted())
            .child(row.field.type_label.clone());

        let value_cell: AnyElement = match &row.field.input {
            // A group has no value of its own; the row is a header for the ones below,
            // and says how many those are whether or not they're showing.
            grpc::FormInput::Group => {
                let prefix = format!("{}.", row.field.path);
                let children = self
                    .form_rows
                    .iter()
                    .filter(|r| {
                        r.field.depth == row.field.depth + 1 && r.field.path.starts_with(&prefix)
                    })
                    .count();
                div()
                    .flex_1()
                    .text_size(theme::text_2xs())
                    .text_color(theme::tree_glyph())
                    .child(format!(
                        "{children} field{}",
                        if children == 1 { "" } else { "s" }
                    ))
                    .into_any_element()
            }
            grpc::FormInput::Bool | grpc::FormInput::Enum(_) => {
                let set = row.picked.is_some();
                let label = row.picked.clone().unwrap_or_else(|| "null".into());
                div()
                    .id(SharedString::from(format!("grpc-form-pick-{i}")))
                    .flex_1()
                    .px_1()
                    .rounded(theme::radius_sm())
                    .font_family(theme::mono_font())
                    .text_size(theme::text_xs())
                    .text_color(if set {
                        theme::text_primary()
                    } else {
                        theme::tree_glyph()
                    })
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::row_hover()))
                    .child(label)
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        if let Some(row) = this.form_rows.get_mut(i) {
                            row.cycle();
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            grpc::FormInput::Text { .. } | grpc::FormInput::Json { .. } => div()
                .flex_1()
                .min_w(px(0.))
                .children(row.input.as_ref().map(|s| Input::new(s).small()))
                .into_any_element(),
        };

        let wrapper = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(name_cell)
            .child(type_cell)
            .child(value_cell);
        if !is_group {
            return wrapper.into_any_element();
        }
        // Only a group takes an id, because only a group is clickable — the whole row is
        // the collapse target.
        wrapper
            .id(SharedString::from(format!("grpc-form-group-{i}")))
            .cursor_pointer()
            .hover(|d| d.bg(theme::row_hover()))
            .on_click(cx.listener(move |this, _e, _w, cx| {
                if let Some(row) = this.form_rows.get_mut(i) {
                    row.collapsed = !row.collapsed;
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// An editable metadata key/value table.
    fn metadata_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap(px(2.)).px_2().pb(px(4.));
        for (i, r) in self.metadata_rows.iter().enumerate() {
            let on = r.on;
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!("grpc-md-on-{i}")))
                            .flex_none()
                            .w(px(16.))
                            .text_size(theme::text_sm())
                            .text_color(if on {
                                theme::accent()
                            } else {
                                theme::tree_glyph()
                            })
                            .cursor_pointer()
                            .child(if on { "☑" } else { "☐" })
                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                if let Some(row) = this.metadata_rows.get_mut(i) {
                                    row.on = !row.on;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(Input::new(&r.key).small()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(Input::new(&r.value).small()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("grpc-md-rm-{i}")))
                            .flex_none()
                            .w(px(16.))
                            .text_size(theme::text_xs())
                            .text_color(theme::text_muted())
                            .cursor_pointer()
                            .hover(|d| {
                                d.text_color(theme::status_color(
                                    moonlight_domain::session::SessionStatus::Errored,
                                ))
                            })
                            .child("✕")
                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                if i < this.metadata_rows.len() {
                                    this.metadata_rows.remove(i);
                                }
                                cx.notify();
                            })),
                    ),
            );
        }
        col.child(
            div()
                .id("grpc-md-add")
                .flex_none()
                .mt(px(1.))
                .px_1()
                .text_size(theme::text_xs())
                .text_color(theme::accent())
                .cursor_pointer()
                .hover(|d| d.text_color(theme::text_primary()))
                .child("＋ add row")
                .on_click(cx.listener(move |this, _e, window, cx| this.add_row(window, cx))),
        )
    }

    /// Read-only view of the selected method's request and response types — the answer to
    /// "what fields does this take?" without leaving the panel.
    fn schema_view(&self) -> impl IntoElement {
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .px_3()
            .pb(px(6.))
            .font_family(theme::mono_font())
            .text_size(theme::text_2xs());

        let (Some(schema), Some(service), Some(method)) =
            (&self.schema, &self.service, &self.method)
        else {
            return col.child(
                div()
                    .font_family(theme::ui_font())
                    .text_size(theme::text_xs())
                    .text_color(theme::text_muted())
                    .child("connect and pick a method to see its message types"),
            );
        };
        let Some((req, resp)) = schema.method_types(service, method) else {
            return col.child(
                div()
                    .font_family(theme::ui_font())
                    .text_size(theme::text_xs())
                    .text_color(theme::text_muted())
                    .child("the selected method is not in the loaded schema"),
            );
        };
        for (label, desc) in [("request", &req), ("response", &resp)] {
            col = col.child(
                div()
                    .pt(px(4.))
                    .text_color(theme::text_muted())
                    .child(format!("{label}  {}", desc.full_name())),
            );
            for field in desc.fields() {
                let kind = grpc::field_kind_label(&field);
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme::text_secondary())
                                .child(field.json_name().to_string()),
                        )
                        .child(div().flex_1().text_color(theme::text_muted()).child(kind)),
                );
            }
        }
        col
    }

    /// The "copied" confirmation, when there is one to show.
    fn copied_flash(&self) -> Option<impl IntoElement> {
        self.copied.clone().map(|what| {
            div()
                .flex_none()
                .text_size(theme::text_2xs())
                .text_color(theme::accent())
                .child(format!("copied {what}"))
        })
    }

    /// The response body as click-to-copy lines.
    ///
    /// One click takes the value on that line — unquoted, comma stripped — so lifting an
    /// id out of a response and into the next request is a click and a paste rather than
    /// a retype. Above [`COPY_LINES`] this falls back to one block of text.
    fn body_lines(&self, body: &str, cx: &mut Context<Self>) -> AnyElement {
        let shown = clip(body, RENDER_BODY);
        let lines: Vec<String> = shown.lines().map(str::to_string).collect();
        if lines.len() > COPY_LINES {
            return div()
                .id("grpc-resp-body")
                .size_full()
                .overflow_scroll()
                .p_2()
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(shown)
                .into_any_element();
        }

        let mut col = div()
            .id("grpc-resp-body")
            .size_full()
            .overflow_scroll()
            .p_2()
            .flex()
            .flex_col()
            .font_family(theme::mono_font())
            .text_size(theme::text_xs())
            .text_color(theme::text_secondary());
        for (i, line) in lines.into_iter().enumerate() {
            let value = grpc::json_line_value(&line);
            col = col.child(
                div()
                    .id(SharedString::from(format!("grpc-body-line-{i}")))
                    .px_1()
                    .rounded(theme::radius_sm())
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::row_hover()))
                    .child(line)
                    .on_click(
                        cx.listener(move |this, _e, _w, cx| this.copy(value.clone(), "value", cx)),
                    ),
            );
        }
        col.into_any_element()
    }

    /// One trailer, clickable to copy its value — servers put the real explanation of a
    /// failure in here more often than in the status message.
    fn trailer_row(
        &self,
        i: usize,
        key: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let copyable = value.to_string();
        div()
            .id(SharedString::from(format!("grpc-trailer-{i}")))
            .flex()
            .flex_row()
            .gap_2()
            .px_1()
            .rounded(theme::radius_sm())
            .cursor_pointer()
            .hover(|d| d.bg(theme::row_hover()))
            .font_family(theme::mono_font())
            .text_size(theme::text_2xs())
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_secondary())
                    .child(format!("{key}:")),
            )
            .child(
                div()
                    .flex_1()
                    .text_color(theme::text_muted())
                    .child(value.to_string()),
            )
            .on_click(cx.listener(move |this, _e, _w, cx| this.copy(copyable.clone(), "value", cx)))
    }

    fn response_region(&self, cx: &mut Context<Self>) -> AnyElement {
        // A pre-send refusal: nothing was sent, so there is no status line to head it —
        // and saying so is itself the most useful thing the header can carry.
        if let Some(err) = &self.error {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .border_t_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py(px(4.))
                        .child(
                            div()
                                .flex_1()
                                .text_size(theme::text_xs())
                                .text_color(theme::text_muted())
                                .child("not sent"),
                        )
                        .children(self.copied_flash())
                        .child(pill(
                            "grpc-copy-error",
                            "⧉ Copy".into(),
                            false,
                            cx.listener(|this, _e, _w, cx| {
                                let Some(text) = this.error.as_ref().map(|e| e.as_block()) else {
                                    return;
                                };
                                this.copy(text, "error", cx);
                            }),
                        )),
                )
                .child(
                    div()
                        .id("grpc-error")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .child(error_card(err)),
                )
                .into_any_element();
        }
        let Some(resp) = &self.response else {
            return div()
                .flex_1()
                .min_h(px(0.))
                .p_3()
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child("Send a call to see the response")
                .into_any_element();
        };

        let failed = theme::status_color(moonlight_domain::session::SessionStatus::Errored);
        let (status_color, status_text) = match (&resp.code, &resp.error) {
            (Some(code), _) => (
                if resp.ok {
                    theme::status_color(moonlight_domain::session::SessionStatus::Done)
                } else {
                    failed
                },
                match resp.code_number {
                    // The number matters: gRPC codes are quoted numerically as often as
                    // by name in server logs and docs. The status *message* moved into
                    // the body, where it has room to be read.
                    Some(n) if !resp.ok => format!("{code} ({n})"),
                    _ => code.clone(),
                },
            ),
            // A transport failure never got a status, so saying "error" beats inventing
            // a code that the server never sent.
            (None, Some(_)) => (failed, "no status".into()),
            (None, None) => (theme::text_muted(), "no response".into()),
        };

        let body_view: AnyElement = match self.resp_tab {
            // The response pane shows whichever of the three a call produced: a failure
            // that never reached the server, a non-OK status the server chose to return,
            // or a message.
            RespTab::Response => match (&resp.error, resp.ok) {
                (Some(err), _) => div()
                    .id("grpc-resp-error")
                    .size_full()
                    .overflow_scroll()
                    .child(error_card(err))
                    .into_any_element(),
                (None, false) => div()
                    .id("grpc-resp-status")
                    .size_full()
                    .overflow_scroll()
                    .child(status_card(resp))
                    .into_any_element(),
                (None, true) => self.body_lines(&resp.body, cx),
            },
            RespTab::Trailers => div()
                .id("grpc-resp-meta")
                .size_full()
                .overflow_scroll()
                .p_2()
                .flex()
                .flex_col()
                .gap(px(1.))
                .children(
                    resp.metadata
                        .iter()
                        .enumerate()
                        .map(|(i, (k, v))| self.trailer_row(i, k, v, cx)),
                )
                .into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py(px(4.))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(status_color)
                            .text_size(theme::text_sm())
                            .child(status_text),
                    )
                    .child(meta(format!("{} ms", resp.ms)))
                    .child(meta(format!("{} B", resp.bytes)))
                    .child(div().flex_1().min_w(px(0.)))
                    .children(self.copied_flash())
                    // Resolved on click, not captured: a 200 KB body would otherwise be
                    // cloned into this closure on every render.
                    .child(pill(
                        "grpc-copy-resp",
                        "⧉ Copy".into(),
                        false,
                        cx.listener(|this, _e, _w, cx| {
                            let Some(resp) = this.response.as_ref() else {
                                return;
                            };
                            let (text, label) = this.response_text(resp);
                            this.copy(text, label, cx);
                        }),
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .px_2()
                    .pb(px(2.))
                    .child(tab_chip(
                        "grpc-r-tab-body",
                        "Response",
                        self.resp_tab == RespTab::Response,
                        cx.listener(|this, _e, _w, cx| {
                            this.resp_tab = RespTab::Response;
                            cx.notify();
                        }),
                    ))
                    .child(tab_chip(
                        "grpc-r-tab-meta",
                        &format!("Trailers ({})", resp.metadata.len()),
                        self.resp_tab == RespTab::Trailers,
                        cx.listener(|this, _e, _w, cx| {
                            this.resp_tab = RespTab::Trailers;
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(body_view),
            )
            .into_any_element()
    }
}

/// The panel's own failure when the tokio reactor never started — the one error here
/// that isn't about the server at all.
fn runtime_missing() -> grpc::GrpcError {
    grpc::GrpcError::new(
        "The gRPC runtime isn't running",
        "this panel's tokio reactor failed to start",
    )
    .with_hint("restart MoonlightCode")
}

/// A failure, rendered as what happened / what was said / what to do.
///
/// Three lines with different weights rather than one red paragraph: the title is the
/// answer to "what went wrong", the detail is the evidence, and the hint is the next
/// move. A wall of red text makes the reader find all three for themselves.
fn error_card(err: &grpc::GrpcError) -> impl IntoElement {
    let failed = theme::status_color(moonlight_domain::session::SessionStatus::Errored);
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p_3()
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_start()
                .child(div().flex_none().text_color(failed).child("⚠"))
                .child(
                    div()
                        .flex_1()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_size(theme::text_sm())
                        .text_color(failed)
                        .child(err.title.clone()),
                ),
        )
        .when(!err.detail.is_empty(), |d| {
            d.child(
                div()
                    .pl(px(20.))
                    .font_family(theme::mono_font())
                    .text_size(theme::text_2xs())
                    .text_color(theme::text_muted())
                    .child(err.detail.clone()),
            )
        })
        .children(err.hint.clone().map(|hint| {
            div()
                .pl(px(20.))
                .pt(px(2.))
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(format!("→ {hint}"))
        }))
}

/// A non-OK status: the server's own answer, not a failure of the call.
///
/// Kept visibly distinct from [`error_card`] because the difference matters — the request
/// was well-formed and the server received it, so nothing about the panel's own
/// configuration is suspect.
fn status_card(resp: &GrpcOutcome) -> impl IntoElement {
    let code = resp.code.clone().unwrap_or_default();
    let hint = resp
        .code_number
        .map(tonic::Code::from_i32)
        .and_then(grpc::status_hint);

    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p_3()
        .child(
            div()
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child(format!(
                    "{code} — the server answered, and refused the call"
                )),
        )
        .when(!resp.status_message.is_empty(), |d| {
            d.child(
                div()
                    .font_family(theme::mono_font())
                    .text_size(theme::text_xs())
                    .text_color(theme::text_primary())
                    .child(resp.status_message.clone()),
            )
        })
        .when(resp.status_message.is_empty(), |d| {
            d.child(
                div()
                    .text_size(theme::text_xs())
                    .text_color(theme::tree_glyph())
                    .child("(no status message)"),
            )
        })
        .children(hint.map(|hint| {
            div()
                .pt(px(2.))
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(format!("→ {hint}"))
        }))
        // Servers routinely put the real explanation in a trailer; say so rather than
        // leaving the operator to find the tab.
        .when(!resp.metadata.is_empty(), |d| {
            d.child(
                div()
                    .pt(px(4.))
                    .text_size(theme::text_2xs())
                    .text_color(theme::text_muted())
                    .child(format!(
                        "{} trailer{} came back with it",
                        resp.metadata.len(),
                        if resp.metadata.len() == 1 { "" } else { "s" }
                    )),
            )
        })
}

/// The form's empty states — never a blank pane.
fn form_placeholder(text: impl Into<String>) -> AnyElement {
    div()
        .px_3()
        .pb(px(6.))
        .text_size(theme::text_xs())
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// The Form/JSON switch. A pair of chips rather than a dropdown: two states, both worth
/// showing, and the inactive one names what you'd get.
fn mode_chip(
    id: &'static str,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .px(px(6.))
        .py(px(2.))
        .rounded(theme::radius_sm())
        .text_size(theme::text_2xs())
        .text_color(if active {
            theme::text_primary()
        } else {
            theme::text_muted()
        })
        .cursor_pointer()
        .when(active, |d| d.bg(theme::tint(theme::accent(), 0.12)))
        .when(!active, |d| d.hover(|d| d.bg(theme::row_hover())))
        .on_click(on_click)
        .child(label)
}

fn meta(text: String) -> impl IntoElement {
    div()
        .flex_none()
        .text_size(theme::text_2xs())
        .text_color(theme::text_muted())
        .child(text)
}

/// A request/response sub-tab chip.
fn tab_chip(
    id: &'static str,
    label: &str,
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let color = if active {
        theme::text_primary()
    } else {
        theme::text_muted()
    };
    div()
        .id(id)
        .px_2()
        .py(px(2.))
        .rounded(theme::radius_sm())
        .text_size(theme::text_xs())
        .text_color(color)
        .cursor_pointer()
        .when(active, |d| d.bg(theme::tint(theme::accent(), 0.12)))
        .when(!active, |d| d.hover(|d| d.bg(theme::row_hover())))
        .on_click(on_click)
        .child(label.to_string())
}
