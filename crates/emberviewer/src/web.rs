//! The browser client: a lean egui UI that mirrors one provider at a time over
//! the WebSocket. It reuses the shared [`TreeModel`] and command vocabulary; the
//! heavier desktop chrome (address book, matrices, meters) is desktop-only for
//! now — this focuses on browsing the tree and viewing/setting values.
#![allow(deprecated)] // egui Panel aliases, as in `app` (migrate when settled)

use std::collections::{HashMap, HashSet};

use ember_proto::glow::{self, Value};
use ember_web_proto::{ClientMsg, WireProvider, WireStatus};

use crate::matrix_view;
use crate::model::{format_value, Kind, MatrixInfo, TreeModel};
use crate::net::NetCommand;
use crate::web_transport::{WebEvent, WsConnection};

const ACCENT: egui::Color32 = egui::Color32::from_rgb(217, 119, 43);

pub struct WebApp {
    conn: Option<WsConnection>,
    authed: bool,
    open_lan: bool,
    auth_error: Option<String>,
    closed: bool,
    providers: Vec<WireProvider>,
    current: Option<u64>,
    tree: TreeModel,
    status: Option<WireStatus>,
    /// Nodes we've already asked to expand (so we fetch each once).
    requested: HashSet<Vec<u32>>,
    /// Matrix-related sub-trees already requested (matrix dir, labels, params).
    matrix_fetch: HashSet<Vec<u32>>,
    /// Parameters we hold a subscription for.
    subscribed: HashSet<Vec<u32>>,
    /// In-progress string edits, keyed by path.
    edits: HashMap<Vec<u32>, String>,
    /// Last "denied" message from the server (read-only mode).
    denied: Option<String>,
    dark: bool,
}

impl WebApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let token = url_query_param("token");
        let conn = ws_url().and_then(|url| WsConnection::connect(&url, cc.egui_ctx.clone()));
        if let Some(c) = &conn {
            c.send(ClientMsg::Auth { token });
        }
        WebApp {
            conn,
            authed: false,
            open_lan: false,
            auth_error: None,
            closed: false,
            providers: Vec::new(),
            current: None,
            tree: TreeModel::new(),
            status: None,
            requested: HashSet::new(),
            matrix_fetch: HashSet::new(),
            subscribed: HashSet::new(),
            edits: HashMap::new(),
            denied: None,
            dark: true,
        }
    }

    fn open_provider(&mut self, id: u64) {
        self.current = Some(id);
        self.tree = TreeModel::new();
        self.requested.clear();
        self.matrix_fetch.clear();
        self.subscribed.clear();
        self.status = None;
        if let Some(c) = &self.conn {
            c.send(ClientMsg::OpenProvider { id });
        }
    }

    fn apply_inbound(&mut self) {
        let events = self.conn.as_ref().map(|c| c.drain()).unwrap_or_default();
        for ev in events {
            match ev {
                WebEvent::AuthOk { open_lan } => {
                    self.authed = true;
                    self.open_lan = open_lan;
                    self.auth_error = None;
                }
                WebEvent::AuthRejected => {
                    self.auth_error = Some("Access denied — check the token in the URL.".into());
                }
                WebEvent::Providers(list) => self.providers = list,
                WebEvent::Status { id, status } => {
                    if self.current == Some(id) {
                        self.status = Some(status);
                    }
                }
                WebEvent::Document { id, root } => {
                    if self.current == Some(id) {
                        self.tree.merge(root);
                    }
                }
                WebEvent::Denied { reason } => self.denied = Some(reason),
                WebEvent::Closed => self.closed = true,
            }
        }
    }
}

impl eframe::App for WebApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Theme (re-applied each frame; cheap and avoids startup override issues).
        let mut v = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.selection.bg_fill = ACCENT.gamma_multiply(if self.dark { 0.55 } else { 0.40 });
        v.selection.stroke.color = ACCENT;
        v.hyperlink_color = ACCENT;
        ui.ctx().set_visuals(v);

        self.apply_inbound();

        egui::TopBottomPanel::top("webbar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("emberviewer").color(ACCENT));
                ui.separator();
                if self.authed {
                    let mut pending: Option<u64> = None;
                    let current_name = self
                        .current
                        .and_then(|id| self.providers.iter().find(|p| p.id == id))
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| "Pick a provider…".into());
                    egui::ComboBox::from_id_salt("provider")
                        .selected_text(current_name)
                        .show_ui(ui, |ui| {
                            for p in &self.providers {
                                if ui
                                    .selectable_label(self.current == Some(p.id), &p.name)
                                    .clicked()
                                {
                                    pending = Some(p.id);
                                }
                            }
                        });
                    if let Some(id) = pending {
                        self.open_provider(id);
                    }
                    if let Some(st) = &self.status {
                        ui.separator();
                        ui.label(status_text(st));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(if self.dark { "☀" } else { "🌙" })
                        .on_hover_text("Toggle theme")
                        .clicked()
                    {
                        self.dark = !self.dark;
                    }
                    if self.open_lan {
                        ui.label(egui::RichText::new("open-LAN").small().weak());
                    }
                });
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(err) = &self.auth_error {
                ui.colored_label(egui::Color32::LIGHT_RED, err);
                return;
            }
            if !self.authed {
                ui.label("Connecting…");
                return;
            }
            if self.closed {
                ui.colored_label(egui::Color32::LIGHT_RED, "Connection closed.");
            }
            if let Some(d) = &self.denied {
                ui.colored_label(egui::Color32::YELLOW, format!("Denied: {d}"));
            }
            if self.current.is_none() {
                ui.label("Choose a provider above to start browsing.");
                return;
            }

            let mut commands: Vec<NetCommand> = Vec::new();
            let mut visible: Vec<Vec<u32>> = Vec::new();
            let roots = self.tree.roots.clone();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if roots.is_empty() {
                        ui.weak("Loading…");
                    }
                    for root in &roots {
                        render_node(
                            ui,
                            &self.tree,
                            root,
                            &mut self.requested,
                            &mut self.matrix_fetch,
                            &mut self.edits,
                            &mut commands,
                            &mut visible,
                        );
                    }
                });

            // Reconcile subscriptions with what's on screen.
            let visible: HashSet<Vec<u32>> = visible.into_iter().collect();
            for p in &visible {
                if !self.subscribed.contains(p) {
                    commands.push(NetCommand::Subscribe(p.clone()));
                }
            }
            for p in &self.subscribed {
                if !visible.contains(p) {
                    commands.push(NetCommand::Unsubscribe(p.clone()));
                }
            }
            self.subscribed = visible;

            if let (Some(conn), Some(id)) = (&self.conn, self.current) {
                for cmd in &commands {
                    conn.send_command(id, cmd);
                }
            }
        });
    }
}

/// Render one tree entry (recursively). Pushes commands and records visible
/// parameter paths.
#[allow(clippy::too_many_arguments)]
fn render_node(
    ui: &mut egui::Ui,
    tree: &TreeModel,
    path: &[u32],
    requested: &mut HashSet<Vec<u32>>,
    mfetch: &mut HashSet<Vec<u32>>,
    edits: &mut HashMap<Vec<u32>, String>,
    commands: &mut Vec<NetCommand>,
    visible: &mut Vec<Vec<u32>>,
) {
    let Some(entry) = tree.get(path) else {
        return;
    };

    if entry.kind.is_expandable() {
        let label = entry.label();

        // Matrix: render the crosspoint grid in the header body (shared with the
        // desktop). Fetch its directory + label/param sub-trees on first show.
        if let (Kind::Matrix, Some(m)) = (entry.kind, &entry.matrix) {
            egui::CollapsingHeader::new(egui::RichText::new(label).strong())
                .id_salt(path)
                .show(ui, |ui| {
                    fetch_matrix(tree, path, m, mfetch, commands);
                    let _ = matrix_view::render_matrix(ui, entry, m, true, commands);
                });
            return;
        }

        // Function: rich UI is desktop-only for now.
        let suffix = if matches!(entry.kind, Kind::Function) {
            "  (function — desktop)"
        } else {
            ""
        };
        let heading = if entry.is_online {
            egui::RichText::new(format!("{label}{suffix}"))
        } else {
            egui::RichText::new(format!("{label}{suffix} (offline)")).weak()
        };
        let header = egui::CollapsingHeader::new(heading)
            .id_salt(path)
            .show(ui, |ui| {
                let children = entry.children.clone();
                if children.is_empty() {
                    ui.weak("…");
                }
                for child in &children {
                    render_node(ui, tree, child, requested, mfetch, edits, commands, visible);
                }
            });
        // On first expansion, fetch this node's children.
        if header.openness > 0.0 && requested.insert(path.to_vec()) {
            commands.push(NetCommand::GetDirectory(path.to_vec()));
        }
        return;
    }

    // A leaf parameter.
    visible.push(path.to_vec());
    ui.horizontal(|ui| {
        let (badge, color) = if entry.is_writable() {
            ("rw", egui::Color32::from_rgb(70, 130, 200))
        } else {
            ("ro", egui::Color32::from_gray(140))
        };
        ui.label(egui::RichText::new(badge).monospace().small().color(color));
        ui.label(egui::RichText::new(entry.label()).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            param_editor(ui, tree, path, edits, commands);
        });
    });
}

/// A compact value editor / display for the parameter at `path`.
fn param_editor(
    ui: &mut egui::Ui,
    tree: &TreeModel,
    path: &[u32],
    edits: &mut HashMap<Vec<u32>, String>,
    commands: &mut Vec<NetCommand>,
) {
    let Some(entry) = tree.get(path) else {
        return;
    };
    let writable = entry.is_writable();
    let set = |commands: &mut Vec<NetCommand>, v: Value| {
        commands.push(NetCommand::SetValue(path.to_vec(), v));
    };

    // Trigger.
    if entry.param_type == Some(glow::parameter_type::TRIGGER) {
        if ui.button("Fire").clicked() {
            set(commands, Value::Integer(0));
        }
        return;
    }

    match &entry.value {
        Some(Value::Boolean(b)) => {
            let mut on = *b;
            if writable {
                if ui.checkbox(&mut on, "").changed() {
                    set(commands, Value::Boolean(on));
                }
            } else {
                ui.label(if on { "true" } else { "false" });
            }
        }
        Some(Value::Integer(i)) if !entry.enum_entries.is_empty() => {
            // Enum.
            let current = entry
                .enum_label(*i)
                .map(str::to_string)
                .unwrap_or_else(|| i.to_string());
            if writable {
                egui::ComboBox::from_id_salt(path)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for e in entry.enum_entries.iter().filter(|e| !e.hidden) {
                            if ui.selectable_label(e.value == *i, &e.label).clicked() {
                                set(commands, Value::Integer(e.value));
                            }
                        }
                    });
            } else {
                ui.label(current);
            }
        }
        Some(Value::Integer(i)) => {
            if writable {
                let mut v = *i;
                if ui.add(egui::DragValue::new(&mut v)).changed() {
                    set(commands, Value::Integer(v));
                }
            } else {
                ui.label(i.to_string());
            }
        }
        Some(Value::Real(r)) => {
            if writable {
                let mut v = r.to_f64();
                if ui.add(egui::DragValue::new(&mut v).speed(0.1)).changed() {
                    set(commands, Value::Real(v.into()));
                }
            } else {
                ui.label(format!("{}", r.to_f64()));
            }
        }
        Some(Value::String(s)) => {
            if writable {
                let buf = edits.entry(path.to_vec()).or_insert_with(|| s.clone());
                let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(160.0));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let v = buf.clone();
                    set(commands, Value::String(v));
                }
            } else {
                ui.label(s.clone());
            }
        }
        Some(v) => {
            ui.label(format_value(v));
        }
        None => {
            ui.weak("—");
        }
    }
}

fn status_text(st: &WireStatus) -> String {
    match st {
        WireStatus::Connecting => "connecting…".into(),
        WireStatus::Connected => "connected".into(),
        WireStatus::Reconnecting { secs, reason } => format!("reconnecting in {secs}s · {reason}"),
        WireStatus::Disconnected { reason } => {
            format!("disconnected · {}", reason.as_deref().unwrap_or("closed"))
        }
        WireStatus::Error { message } => format!("error · {message}"),
    }
}

/// Fetch the data a matrix grid needs: the matrix's own directory (targets /
/// sources / connections) and its label / parameter sub-trees. Deduped via
/// `mfetch`; the label children fetch once they appear in the tree.
fn fetch_matrix(
    tree: &TreeModel,
    path: &[u32],
    m: &MatrixInfo,
    mfetch: &mut HashSet<Vec<u32>>,
    commands: &mut Vec<NetCommand>,
) {
    if mfetch.insert(path.to_vec()) {
        commands.push(NetCommand::GetMatrixDirectory(path.to_vec()));
    }
    for base in &m.label_paths {
        fetch_subtree(tree, base, mfetch, commands);
    }
    if let Some(ploc) = &m.params_location {
        fetch_subtree(tree, ploc, mfetch, commands);
    }
}

/// Fetch a sub-tree base and (once known) its immediate children.
fn fetch_subtree(
    tree: &TreeModel,
    base: &[u32],
    mfetch: &mut HashSet<Vec<u32>>,
    commands: &mut Vec<NetCommand>,
) {
    if mfetch.insert(base.to_vec()) {
        commands.push(NetCommand::GetDirectory(base.to_vec()));
    }
    if let Some(e) = tree.get(base) {
        for child in &e.children {
            if mfetch.insert(child.clone()) {
                commands.push(NetCommand::GetDirectory(child.clone()));
            }
        }
    }
}

/// Build the WebSocket URL from the page location (`ws[s]://host/ws`).
fn ws_url() -> Option<String> {
    let loc = web_sys::window()?.location();
    let host = loc.host().ok()?;
    let proto = if loc.protocol().ok().as_deref() == Some("https:") {
        "wss:"
    } else {
        "ws:"
    };
    Some(format!("{proto}//{host}/ws"))
}

/// Read a query parameter from the page URL.
fn url_query_param(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let query = search.strip_prefix('?').unwrap_or(&search);
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}
