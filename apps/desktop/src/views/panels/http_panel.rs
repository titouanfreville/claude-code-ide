//! HTTP panel — a Postman-style request builder + response viewer (center tab).
//!
//! Opened from the Services view's HTTP summary ("open ▸"). Build a request (method +
//! URL with `{{vars}}` + Headers/Body), pick an **environment** from the project's
//! `.moonlight/http/environments.json`, and **Send** — the request interpolates the
//! env's variables, is host-scoped (the same SSRF gate as the agent's `http_request`
//! verb), runs on a background task, and lands in the **shared** [`HttpHistory`] so the
//! operator's and the agent's calls share one history. Requests can be **saved** to
//! `.moonlight/http/requests.json` and reloaded.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    MouseDownEvent, SharedString, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, TabPanel};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable;

use super::CloseTab;
use crate::http::{self, HttpCall, HttpOutcome};
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// The methods the method button cycles through.
const METHODS: [&str; 6] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];
/// Cap the response body actually rendered (the stored body is already ≤ 200 KB).
const RENDER_BODY: usize = 50_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RespTab {
    Body,
    Headers,
}

/// How the request body is composed (drives the editor + the `Content-Type`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyFormat {
    None,
    Raw,
    Json,
    Xml,
    Form,
}

impl BodyFormat {
    const ALL: [BodyFormat; 5] = [
        BodyFormat::None,
        BodyFormat::Raw,
        BodyFormat::Json,
        BodyFormat::Xml,
        BodyFormat::Form,
    ];
    fn label(self) -> &'static str {
        match self {
            BodyFormat::None => "None",
            BodyFormat::Raw => "Raw",
            BodyFormat::Json => "JSON",
            BodyFormat::Xml => "XML",
            BodyFormat::Form => "Form",
        }
    }
    /// The `Content-Type` this format sets (unless the operator added one explicitly).
    fn content_type(self) -> Option<&'static str> {
        match self {
            BodyFormat::Json => Some("application/json"),
            BodyFormat::Xml => Some("application/xml"),
            BodyFormat::Form => Some("application/x-www-form-urlencoded"),
            BodyFormat::None | BodyFormat::Raw => None,
        }
    }
}

/// How the JSON body is authored: a key/value **object builder** table, or **raw**
/// free-text JSON.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonMode {
    Table,
    Raw,
}

/// Which key/value table a row operation targets (each has its own `Vec<KvRow>`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tbl {
    Headers,
    Form,
    Json,
    /// The environment editor's variables table.
    Vars,
}

impl Tbl {
    /// Stable element-id prefix so the tables' rows never collide.
    fn prefix(self) -> &'static str {
        match self {
            Tbl::Headers => "h",
            Tbl::Form => "f",
            Tbl::Json => "j",
            Tbl::Vars => "v",
        }
    }
}

/// One editable key/value table row (headers / form fields / JSON object). Each cell is
/// its own `InputState`; `on` is the row's enable checkbox.
struct KvRow {
    key: Entity<InputState>,
    value: Entity<InputState>,
    on: bool,
}

pub struct HttpPanel {
    method: String,
    /// Inputs are stood up lazily (they need a `Window`).
    url_input: Option<Entity<InputState>>,
    /// Headers as an editable key/value table.
    header_rows: Vec<KvRow>,
    body_format: BodyFormat,
    /// Free-text body editor (Raw / XML / JSON-raw).
    body_input: Option<Entity<InputState>>,
    /// Form fields (Form body) as a key/value table.
    form_rows: Vec<KvRow>,
    /// JSON object fields (JSON body, table mode) as a key/value table.
    json_rows: Vec<KvRow>,
    /// JSON body authoring mode (table object-builder vs raw text).
    json_mode: JsonMode,
    /// Selected environment name; `None` = the manifest's active/default env.
    env: Option<String>,
    /// Whether the environment editor panel is open.
    env_editor_open: bool,
    /// The env name currently loaded in the editor (`None` = a fresh, unsaved env).
    env_editing: Option<String>,
    env_name_input: Option<Entity<InputState>>,
    /// The editor's variables table.
    env_var_rows: Vec<KvRow>,
    /// The editor's allowed-hosts editor (one host per line; empty = loopback/private).
    env_hosts_input: Option<Entity<InputState>>,
    response: Option<HttpOutcome>,
    resp_tab: RespTab,
    loading: bool,
    /// A pre-send refusal (empty URL, host not allowed) shown in place of a response.
    error: Option<String>,
    saved: Vec<http::SavedRequest>,
    saved_open: bool,
    /// First render loads saved requests + seeds the env / the first header row.
    needs_init: bool,
    focus_handle: FocusHandle,
    tab_panel: Option<WeakEntity<TabPanel>>,
    tab_menu: Option<super::TabMenu>,
}

impl HttpPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            method: "GET".into(),
            url_input: None,
            header_rows: Vec::new(),
            body_format: BodyFormat::None,
            body_input: None,
            form_rows: Vec::new(),
            json_rows: Vec::new(),
            json_mode: JsonMode::Table,
            env: None,
            env_editor_open: false,
            env_editing: None,
            env_name_input: None,
            env_var_rows: Vec::new(),
            env_hosts_input: None,
            response: None,
            resp_tab: RespTab::Body,
            loading: false,
            error: None,
            saved: Vec::new(),
            saved_open: false,
            needs_init: true,
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
        }
    }

    /// Build one key/value table row, seeded with `k`/`v` (needs a `Window` for the
    /// cell inputs).
    fn make_row(k: &str, v: &str, window: &mut Window, cx: &mut Context<Self>) -> KvRow {
        let key = cx.new(|cx| InputState::new(window, cx).placeholder("Key"));
        let value = cx.new(|cx| InputState::new(window, cx).placeholder("Value"));
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

    fn rows(&self, t: Tbl) -> &Vec<KvRow> {
        match t {
            Tbl::Headers => &self.header_rows,
            Tbl::Form => &self.form_rows,
            Tbl::Json => &self.json_rows,
            Tbl::Vars => &self.env_var_rows,
        }
    }

    fn rows_mut(&mut self, t: Tbl) -> &mut Vec<KvRow> {
        match t {
            Tbl::Headers => &mut self.header_rows,
            Tbl::Form => &mut self.form_rows,
            Tbl::Json => &mut self.json_rows,
            Tbl::Vars => &mut self.env_var_rows,
        }
    }

    fn add_row(&mut self, t: Tbl, window: &mut Window, cx: &mut Context<Self>) {
        let row = Self::make_row("", "", window, cx);
        self.rows_mut(t).push(row);
        cx.notify();
    }

    /// Collect a table's enabled, non-empty rows into pairs, interpolating values.
    fn collect_rows(
        rows: &[KvRow],
        vars: &std::collections::BTreeMap<String, String>,
        cx: &App,
    ) -> Vec<(String, String)> {
        rows.iter()
            .filter(|r| r.on)
            .filter_map(|r| {
                let k = r.key.read(cx).value().trim().to_string();
                if k.is_empty() {
                    return None;
                }
                let raw = r.value.read(cx).value();
                Some((k, http::interpolate(&raw, vars)))
            })
            .collect()
    }

    /// Layout-restore is a scratch builder — start fresh.
    pub fn restore(_info: &PanelInfo, cx: &mut Context<Self>) -> Self {
        Self::new(cx)
    }

    /// One HTTP tab (dedup key shared with `OpenRequest::Http`).
    pub fn tab_key() -> &'static str {
        "http"
    }

    fn project_root(cx: &App) -> Option<PathBuf> {
        cx.try_global::<ShellDeps>()
            .map(|d| d.focus.read(cx).root())
    }

    fn manifest(cx: &App) -> http::HttpManifest {
        Self::project_root(cx)
            .map(|r| http::load_manifest(&r))
            .unwrap_or_default()
    }

    /// The environment whose vars + allowed-hosts drive Send (selected name, else the
    /// manifest's active/default).
    fn active_env(&self, cx: &App) -> http::HttpEnv {
        let manifest = Self::manifest(cx);
        match self.env.as_ref().and_then(|n| manifest.environments.get(n)) {
            Some(e) => e.clone(),
            None => manifest.active_env(),
        }
    }

    fn env_label(&self, cx: &App) -> String {
        self.env
            .clone()
            .or_else(|| Self::manifest(cx).active_name())
            .unwrap_or_else(|| "local only".into())
    }

    /// Toggle the environment editor; on open, load the active env (or a blank one).
    fn toggle_env_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.env_editor_open = !self.env_editor_open;
        if self.env_editor_open {
            let active = self
                .env
                .clone()
                .or_else(|| Self::manifest(cx).active_name());
            self.load_env_into_editor(active, window, cx);
        }
        cx.notify();
    }

    /// Load an environment (or a blank new one when `name` is `None`) into the editor.
    fn load_env_into_editor(
        &mut self,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let manifest = Self::manifest(cx);
        let env = name
            .as_ref()
            .and_then(|n| manifest.environments.get(n).cloned())
            .unwrap_or_default();
        self.env_editing = name.clone();
        if let Some(i) = &self.env_name_input {
            i.update(cx, |s, cx| {
                s.set_value(name.clone().unwrap_or_default(), window, cx)
            });
        }
        self.env_var_rows = env
            .vars
            .iter()
            .map(|(k, v)| Self::make_row(k, v, window, cx))
            .collect();
        if self.env_var_rows.is_empty() {
            self.env_var_rows.push(Self::make_row("", "", window, cx));
        }
        if let Some(i) = &self.env_hosts_input {
            i.update(cx, |s, cx| {
                s.set_value(env.allowed_hosts.join("\n"), window, cx)
            });
        }
        cx.notify();
    }

    /// Persist the editor's environment (upsert by name; makes it active). A rename
    /// (the loaded name changed) drops the old entry.
    fn save_env(&mut self, cx: &mut Context<Self>) {
        let Some(root) = Self::project_root(cx) else {
            return;
        };
        let name = Self::input_text(&self.env_name_input, cx)
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        let no_vars = std::collections::BTreeMap::new();
        let vars: std::collections::BTreeMap<String, String> =
            Self::collect_rows(&self.env_var_rows, &no_vars, cx)
                .into_iter()
                .collect();
        let allowed_hosts: Vec<String> = Self::input_text(&self.env_hosts_input, cx)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let mut manifest = Self::manifest(cx);
        if let Some(old) = &self.env_editing {
            if old != &name {
                manifest.environments.remove(old);
            }
        }
        manifest.environments.insert(
            name.clone(),
            http::HttpEnv {
                vars,
                allowed_hosts,
            },
        );
        manifest.active = name.clone();
        let _ = http::save_manifest(&root, &manifest);
        self.env = Some(name.clone());
        self.env_editing = Some(name);
        cx.notify();
    }

    /// Delete the loaded environment from the manifest.
    fn delete_env(&mut self, cx: &mut Context<Self>) {
        let (Some(root), Some(name)) = (Self::project_root(cx), self.env_editing.clone()) else {
            return;
        };
        let mut manifest = Self::manifest(cx);
        manifest.environments.remove(&name);
        if manifest.active == name {
            manifest.active = manifest
                .environments
                .keys()
                .next()
                .cloned()
                .unwrap_or_default();
        }
        let _ = http::save_manifest(&root, &manifest);
        self.env = None;
        self.env_editing = None;
        self.env_editor_open = false;
        cx.notify();
    }

    fn cycle_method(&mut self, cx: &mut Context<Self>) {
        let i = METHODS.iter().position(|m| *m == self.method).unwrap_or(0);
        self.method = METHODS[(i + 1) % METHODS.len()].to_string();
        cx.notify();
    }

    fn input_text(input: &Option<Entity<InputState>>, cx: &App) -> String {
        input
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Build → interpolate → host-scope → send (off the UI thread) → record in history.
    fn send(&mut self, cx: &mut Context<Self>) {
        let url_raw = Self::input_text(&self.url_input, cx).trim().to_string();
        if url_raw.is_empty() {
            self.error = Some("enter a URL".into());
            cx.notify();
            return;
        }
        let env = self.active_env(cx);
        let url = http::interpolate(&url_raw, &env.vars);
        if !http::host_allowed(&url, &env.allowed_hosts) {
            self.error = Some(format!(
                "host not allowed by the active environment: {url} — add it to \
                 allowed_hosts in .moonlight/http/environments.json"
            ));
            self.response = None;
            cx.notify();
            return;
        }
        let mut headers = Self::collect_rows(&self.header_rows, &env.vars, cx);
        // Body per the chosen format.
        let body = match self.body_format {
            BodyFormat::None => None,
            BodyFormat::Json if self.json_mode == JsonMode::Table => {
                let pairs = Self::collect_rows(&self.json_rows, &env.vars, cx);
                (!pairs.is_empty()).then(|| http::json_object_from_pairs(&pairs))
            }
            BodyFormat::Raw | BodyFormat::Json | BodyFormat::Xml => {
                let raw = Self::input_text(&self.body_input, cx);
                (!raw.trim().is_empty()).then(|| http::interpolate(&raw, &env.vars))
            }
            BodyFormat::Form => {
                let pairs = Self::collect_rows(&self.form_rows, &env.vars, cx);
                (!pairs.is_empty()).then(|| http::form_urlencode(&pairs))
            }
        };
        // Auto Content-Type for the format, unless the operator set one explicitly.
        if body.is_some() {
            if let Some(ct) = self.body_format.content_type() {
                if !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    headers.push(("Content-Type".to_string(), ct.to_string()));
                }
            }
        }
        let spec = http::RequestSpec {
            method: self.method.clone(),
            url: url.clone(),
            headers,
            body,
        };
        let method = self.method.clone();
        let history = cx.try_global::<ShellDeps>().map(|d| d.http_history.clone());

        self.loading = true;
        self.error = None;
        self.response = None;
        cx.notify();
        cx.spawn(async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { http::send(&spec) })
                .await;
            if let Some(h) = history {
                h.record(HttpCall {
                    method,
                    url,
                    status: outcome.status,
                    ms: outcome.ms,
                    ok: outcome.ok,
                    bytes: outcome.bytes,
                    at_millis: now_millis(),
                    error: outcome.error.clone(),
                });
            }
            let _ = weak.update(cx, |this, cx| {
                this.loading = false;
                this.resp_tab = RespTab::Body;
                this.response = Some(outcome);
                cx.notify();
            });
        })
        .detach();
    }

    /// Save the current request to `requests.json` (upsert by `METHOD url` name).
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(root) = Self::project_root(cx) else {
            return;
        };
        let url = Self::input_text(&self.url_input, cx).trim().to_string();
        if url.is_empty() {
            return;
        }
        // Headers from the table; body = the effective string for the current format.
        let no_vars = std::collections::BTreeMap::new();
        let headers = Self::collect_rows(&self.header_rows, &no_vars, cx);
        let body = match self.body_format {
            BodyFormat::None => None,
            BodyFormat::Json if self.json_mode == JsonMode::Table => {
                let pairs = Self::collect_rows(&self.json_rows, &no_vars, cx);
                (!pairs.is_empty()).then(|| http::json_object_from_pairs(&pairs))
            }
            BodyFormat::Form => {
                let pairs = Self::collect_rows(&self.form_rows, &no_vars, cx);
                (!pairs.is_empty()).then(|| http::form_urlencode(&pairs))
            }
            _ => {
                let b = Self::input_text(&self.body_input, cx);
                (!b.trim().is_empty()).then_some(b)
            }
        };
        let req = http::SavedRequest {
            name: clip(&format!("{} {url}", self.method), 60),
            method: self.method.clone(),
            url,
            headers,
            body,
        };
        let mut store = http::load_requests(&root);
        match store.requests.iter_mut().find(|r| r.name == req.name) {
            Some(existing) => *existing = req,
            None => store.requests.push(req),
        }
        let _ = http::save_requests(&root, &store);
        self.saved = store.requests;
        cx.notify();
    }

    /// Load a saved request into the editors.
    fn load_saved(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(req) = self.saved.get(idx).cloned() else {
            return;
        };
        self.method = if req.method.trim().is_empty() {
            "GET".into()
        } else {
            req.method.to_ascii_uppercase()
        };
        if let Some(i) = &self.url_input {
            i.update(cx, |s, cx| s.set_value(req.url.clone(), window, cx));
        }
        // Rebuild the header table from the saved pairs.
        self.header_rows = req
            .headers
            .iter()
            .map(|(k, v)| Self::make_row(k, v, window, cx))
            .collect();
        // Body restores as Raw text (saved requests don't record the format).
        match &req.body {
            Some(b) if !b.trim().is_empty() => {
                self.body_format = BodyFormat::Raw;
                if let Some(i) = &self.body_input {
                    i.update(cx, |s, cx| s.set_value(b.clone(), window, cx));
                }
            }
            _ => self.body_format = BodyFormat::None,
        }
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

impl Focusable for HttpPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl super::TabMenuHost for HttpPanel {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for HttpPanel {}

impl Panel for HttpPanel {
    fn panel_name(&self) -> &'static str {
        "Http"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::tab_title(
            "HTTP".to_string(),
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

impl Render for HttpPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.url_input.is_none() {
            self.url_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder("https://…  or  {{base_url}}/path")
            }));
            self.body_input = Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .auto_grow(4, 16)
                    .placeholder("request body — {{vars}} ok")
            }));
            self.env_name_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder("environment name (e.g. local, staging)")
            }));
            self.env_hosts_input = Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .auto_grow(2, 6)
                    .placeholder("allowed hosts — one per line (empty = loopback/private only)")
            }));
            // Seed one editable row in each table so they're ready to type into.
            self.header_rows.push(Self::make_row("", "", window, cx));
            self.form_rows.push(Self::make_row("", "", window, cx));
            self.json_rows.push(Self::make_row("", "", window, cx));
        }
        if self.needs_init {
            self.needs_init = false;
            if let Some(root) = Self::project_root(cx) {
                self.saved = http::load_requests(&root).requests;
            }
        }

        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        let method = self.method.clone();
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
            // ── Request line: method · URL · Send · env · saved · save ──────────────
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
                    .child(pill(
                        "http-method",
                        method,
                        true,
                        cx.listener(|this, _e, _w, cx| this.cycle_method(cx)),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .children(self.url_input.as_ref().map(Input::new)),
                    )
                    .when(self.loading, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(theme::text_xs())
                                .text_color(theme::text_muted())
                                .child("sending…"),
                        )
                    })
                    .child(pill(
                        "http-send",
                        "Send ▸".into(),
                        true,
                        cx.listener(|this, _e, _w, cx| this.send(cx)),
                    ))
                    .child(pill(
                        "http-env",
                        format!("env: {env_label} ▾"),
                        self.env_editor_open,
                        cx.listener(|this, _e, window, cx| this.toggle_env_editor(window, cx)),
                    ))
                    .child(pill(
                        "http-saved",
                        format!("saved ({saved_count}) ▾"),
                        false,
                        cx.listener(|this, _e, _w, cx| {
                            this.saved_open = !this.saved_open;
                            cx.notify();
                        }),
                    ))
                    .child(pill(
                        "http-save",
                        "＋ Save".into(),
                        false,
                        cx.listener(|this, _e, _w, cx| this.save(cx)),
                    )),
            )
            // Saved-requests list (toggled).
            .when(self.saved_open, |d| d.child(self.saved_list(cx)))
            // Environment editor (toggled).
            .when(self.env_editor_open, |d| d.child(self.env_editor(cx)))
            // ── Request config (Headers + Body) in one scrollable pane ──────────────
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

impl HttpPanel {
    /// The single request-config pane: Headers table + Body (format + editor), in one
    /// scrollable region (capped height).
    fn request_config(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("http-config")
            .flex_none()
            .max_h(px(300.))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(theme::border_subtle())
            .flex()
            .flex_col()
            .child(section_label("Headers"))
            .child(self.kv_table(Tbl::Headers, cx))
            .child(section_label("Body"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pb(px(3.))
                    .children(BodyFormat::ALL.iter().map(|f| {
                        let f = *f;
                        format_chip(
                            f.label(),
                            self.body_format == f,
                            cx.listener(move |this, _e, _w, cx| {
                                this.body_format = f;
                                cx.notify();
                            }),
                        )
                    })),
            )
            .child(self.body_editor(cx))
    }

    /// An editable key/value table for `t` (headers / form body / JSON object).
    fn kv_table(&self, t: Tbl, cx: &mut Context<Self>) -> impl IntoElement {
        let p = t.prefix();
        let mut col = div().flex().flex_col().gap(px(2.)).px_2().pb(px(4.));
        for (i, r) in self.rows(t).iter().enumerate() {
            let on = r.on;
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    // Enable checkbox.
                    .child(
                        div()
                            .id(SharedString::from(format!("kv-{p}-on-{i}")))
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
                                if let Some(row) = this.rows_mut(t).get_mut(i) {
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
                    // Remove row.
                    .child(
                        div()
                            .id(SharedString::from(format!("kv-{p}-rm-{i}")))
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
                                if i < this.rows_mut(t).len() {
                                    this.rows_mut(t).remove(i);
                                }
                                cx.notify();
                            })),
                    ),
            );
        }
        col.child(
            div()
                .id(SharedString::from(format!("kv-{p}-add")))
                .flex_none()
                .mt(px(1.))
                .px_1()
                .text_size(theme::text_xs())
                .text_color(theme::accent())
                .cursor_pointer()
                .hover(|d| d.text_color(theme::text_primary()))
                .child("＋ add row")
                .on_click(cx.listener(move |this, _e, window, cx| this.add_row(t, window, cx))),
        )
    }

    /// The body editor for the current format: a text area, a key/value table, or — for
    /// JSON — a `Table | Raw` sub-toggle over either an object-builder table or raw text.
    fn body_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.body_format {
            BodyFormat::None => div()
                .px_3()
                .py_2()
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child("no body — pick a format above")
                .into_any_element(),
            BodyFormat::Form => self.kv_table(Tbl::Form, cx).into_any_element(),
            BodyFormat::Raw | BodyFormat::Xml => div()
                .px_2()
                .pb(px(4.))
                .children(self.body_input.as_ref().map(Input::new))
                .into_any_element(),
            BodyFormat::Json => {
                let editor = if self.json_mode == JsonMode::Table {
                    self.kv_table(Tbl::Json, cx).into_any_element()
                } else {
                    div()
                        .px_2()
                        .pb(px(4.))
                        .children(self.body_input.as_ref().map(Input::new))
                        .into_any_element()
                };
                div()
                    .flex()
                    .flex_col()
                    // JSON sub-mode toggle.
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .px_2()
                            .pb(px(3.))
                            .child(format_chip(
                                "Table",
                                self.json_mode == JsonMode::Table,
                                cx.listener(|this, _e, _w, cx| {
                                    this.json_mode = JsonMode::Table;
                                    cx.notify();
                                }),
                            ))
                            .child(format_chip(
                                "Raw JSON",
                                self.json_mode == JsonMode::Raw,
                                cx.listener(|this, _e, _w, cx| {
                                    this.json_mode = JsonMode::Raw;
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(editor)
                    .into_any_element()
            }
        }
    }

    /// The environment editor: pick/create an env, edit its variables (a key/value
    /// table) + allowed hosts, then Save (upsert + make active) or Delete.
    fn env_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let names: Vec<String> = Self::manifest(cx).environments.keys().cloned().collect();
        let editing = self.env_editing.clone();
        let env_chip = |i: usize, label: String, active: bool, on_click| {
            div()
                .id(SharedString::from(format!("http-env-chip-{i}")))
                .px_2()
                .py(px(2.))
                .rounded(theme::radius_sm())
                .text_size(theme::text_xs())
                .text_color(if active {
                    theme::text_primary()
                } else {
                    theme::text_muted()
                })
                .cursor_pointer()
                .when(active, |d| d.bg(theme::tint(theme::accent(), 0.14)))
                .when(!active, |d| d.hover(|d| d.bg(theme::row_hover())))
                .on_click(on_click)
                .child(label)
        };
        div()
            .id("http-env-editor")
            .flex()
            .flex_col()
            .gap(px(2.))
            .max_h(px(300.))
            .overflow_y_scroll()
            .py(px(4.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_sunken())
            // Env chips + New.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .flex_wrap()
                    .px_2()
                    .children(names.iter().enumerate().map(|(i, n)| {
                        let n2 = n.clone();
                        env_chip(
                            i,
                            n.clone(),
                            editing.as_deref() == Some(n.as_str()),
                            cx.listener(move |this, _e, window, cx| {
                                this.load_env_into_editor(Some(n2.clone()), window, cx)
                            }),
                        )
                    }))
                    .child(
                        div()
                            .id("http-env-new")
                            .px_2()
                            .py(px(2.))
                            .rounded(theme::radius_sm())
                            .text_size(theme::text_xs())
                            .text_color(theme::accent())
                            .cursor_pointer()
                            .hover(|d| d.bg(theme::row_hover()))
                            .child("＋ New")
                            .on_click(cx.listener(|this, _e, window, cx| {
                                this.load_env_into_editor(None, window, cx)
                            })),
                    ),
            )
            .child(section_label("Name"))
            .child(
                div()
                    .px_2()
                    .children(self.env_name_input.as_ref().map(Input::new)),
            )
            .child(section_label(
                "Variables  ·  use as {{name}} in URL / headers / body",
            ))
            .child(self.kv_table(Tbl::Vars, cx))
            .child(section_label("Allowed hosts"))
            .child(
                div()
                    .px_2()
                    .children(self.env_hosts_input.as_ref().map(Input::new)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .pt(px(4.))
                    .child(pill(
                        "http-env-save",
                        "Save env".into(),
                        true,
                        cx.listener(|this, _e, _w, cx| this.save_env(cx)),
                    ))
                    .when(editing.is_some(), |d| {
                        d.child(pill(
                            "http-env-del",
                            "Delete".into(),
                            false,
                            cx.listener(|this, _e, _w, cx| this.delete_env(cx)),
                        ))
                    })
                    .child(pill(
                        "http-env-close",
                        "Close".into(),
                        false,
                        cx.listener(|this, _e, _w, cx| {
                            this.env_editor_open = false;
                            cx.notify();
                        }),
                    )),
            )
    }

    fn saved_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .id("http-saved-list")
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
                    .child("no saved requests — build one and ＋ Save"),
            );
        }
        for (i, r) in self.saved.iter().enumerate() {
            let name = r.name.clone();
            col = col.child(
                div()
                    .id(("http-saved-row", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
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

    fn response_region(&self, cx: &mut Context<Self>) -> AnyElement {
        // Pre-send refusal.
        if let Some(err) = &self.error {
            return div()
                .flex_1()
                .min_h(px(0.))
                .p_3()
                .text_size(theme::text_xs())
                .text_color(theme::status_color(
                    moonlight_domain::session::SessionStatus::Errored,
                ))
                .child(err.clone())
                .into_any_element();
        }
        let Some(resp) = &self.response else {
            return div()
                .flex_1()
                .min_h(px(0.))
                .p_3()
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child("Send a request to see the response")
                .into_any_element();
        };

        let (status_color, status_text) = match (resp.status, &resp.error) {
            (Some(code), _) => (
                if resp.ok {
                    theme::status_color(moonlight_domain::session::SessionStatus::Done)
                } else {
                    theme::status_color(moonlight_domain::session::SessionStatus::Errored)
                },
                format!("{code} {}", resp.status_text),
            ),
            (None, Some(e)) => (
                theme::status_color(moonlight_domain::session::SessionStatus::Errored),
                format!("error · {e}"),
            ),
            (None, None) => (theme::text_muted(), "no response".into()),
        };

        let body_view: AnyElement = match self.resp_tab {
            RespTab::Body => div()
                .id("http-resp-body")
                .size_full()
                .overflow_scroll()
                .p_2()
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(clip(&resp.body, RENDER_BODY))
                .into_any_element(),
            RespTab::Headers => div()
                .id("http-resp-headers")
                .size_full()
                .overflow_scroll()
                .p_2()
                .flex()
                .flex_col()
                .gap(px(1.))
                .children(resp.headers.iter().map(|(k, v)| header_row(k, v)))
                .into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .border_t_1()
            .border_color(theme::border_subtle())
            // Status line.
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
                    .child(meta(format!("{} B", resp.bytes))),
            )
            // Response tabs.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .px_2()
                    .pb(px(2.))
                    .child(tab_chip(
                        "r-tab-body",
                        "Body",
                        self.resp_tab == RespTab::Body,
                        cx.listener(|this, _e, _w, cx| {
                            this.resp_tab = RespTab::Body;
                            cx.notify();
                        }),
                    ))
                    .child(tab_chip(
                        "r-tab-headers",
                        &format!("Headers ({})", resp.headers.len()),
                        self.resp_tab == RespTab::Headers,
                        cx.listener(|this, _e, _w, cx| {
                            this.resp_tab = RespTab::Headers;
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

fn meta(text: String) -> impl IntoElement {
    div()
        .flex_none()
        .text_size(theme::text_2xs())
        .text_color(theme::text_muted())
        .child(text)
}

fn header_row(k: &str, v: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .font_family(theme::mono_font())
        .text_size(theme::text_2xs())
        .child(
            div()
                .flex_none()
                .text_color(theme::text_secondary())
                .child(format!("{k}:")),
        )
        .child(
            div()
                .flex_1()
                .text_color(theme::text_muted())
                .child(v.to_string()),
        )
}

/// A small uppercased section header inside the request-config pane.
fn section_label(text: &str) -> impl IntoElement {
    div()
        .px_2()
        .pt(px(5.))
        .pb(px(2.))
        .text_size(theme::text_2xs())
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(text.to_uppercase())
}

/// A body-format radio chip (None · Raw · JSON · XML · Form).
fn format_chip(
    label: &str,
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("http-fmt-{label}")))
        .px_2()
        .py(px(2.))
        .rounded(theme::radius_sm())
        .text_size(theme::text_xs())
        .text_color(if active {
            theme::text_primary()
        } else {
            theme::text_muted()
        })
        .cursor_pointer()
        .when(active, |d| d.bg(theme::tint(theme::accent(), 0.14)))
        .when(!active, |d| d.hover(|d| d.bg(theme::row_hover())))
        .on_click(on_click)
        .child(label.to_string())
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
