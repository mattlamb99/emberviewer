//! The browser client: a lean egui UI that mirrors one provider at a time over
//! the WebSocket. It reuses the shared [`TreeModel`] and command vocabulary; the
//! heavier desktop chrome (address book, matrices, meters) is desktop-only for
//! now — this focuses on browsing the tree and viewing/setting values.
#![allow(deprecated)] // egui Panel aliases, as in `app` (migrate when settled)

use std::collections::{HashMap, HashSet};

use ember_proto::glow::{self, Value};
use ember_web_proto::{ClientMsg, WireNode, WireProvider, WireStatus};

use crate::matrix_view;
use crate::model::{format_value, Kind, MatrixInfo, TreeModel};
use crate::net::NetCommand;
use crate::web_transport::{WebEvent, WsConnection};
use crate::widgets;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(217, 119, 43);

pub struct WebApp {
    conn: Option<WsConnection>,
    authed: bool,
    open_lan: bool,
    auth_error: Option<String>,
    closed: bool,
    providers: Vec<WireProvider>,
    /// The address book (folders + providers) for the left pane.
    address_tree: Vec<WireNode>,
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
    /// Function argument input buffers: (function path, arg index) -> text.
    func_inputs: HashMap<(Vec<u32>, usize), String>,
    /// Last invocation id issued per function path.
    invocations: HashMap<Vec<u32>, i32>,
    next_invocation_id: i32,
    /// Auto-tracked value range per meter path.
    meter_range: HashMap<Vec<u32>, (f64, f64)>,
    /// Open "signal parameters" popup (matrix header click): node path + title.
    signal_params: Option<(Vec<u32>, String)>,
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
            address_tree: Vec::new(),
            current: None,
            tree: TreeModel::new(),
            status: None,
            requested: HashSet::new(),
            matrix_fetch: HashSet::new(),
            subscribed: HashSet::new(),
            edits: HashMap::new(),
            func_inputs: HashMap::new(),
            invocations: HashMap::new(),
            next_invocation_id: 1,
            meter_range: HashMap::new(),
            signal_params: None,
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
        self.invocations.clear();
        self.meter_range.clear();
        self.signal_params = None;
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
                WebEvent::AddressBook(nodes) => self.address_tree = nodes,
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

        let mut pending_open: Option<u64> = None;

        egui::TopBottomPanel::top("webbar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("emberviewer").color(ACCENT));
                ui.separator();
                if let Some(st) = &self.status {
                    ui.label(status_text(st));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(if self.dark { "Light" } else { "Dark" })
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

        // Left pane: the address book (folders + providers).
        if self.authed {
            egui::SidePanel::left("providers")
                .resizable(true)
                .default_width(200.0)
                .show_inside(ui, |ui| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Providers").strong());
                    ui.separator();
                    let nodes = self.address_tree.clone();
                    let current = self.current;
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if nodes.is_empty() {
                                ui.weak("(no providers)");
                            }
                            for n in &nodes {
                                render_book_node(ui, n, current, &mut pending_open);
                            }
                        });
                });
        }

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
                ui.label("Choose a provider on the left to start browsing.");
                return;
            }

            let mut commands: Vec<NetCommand> = Vec::new();
            let mut visible: Vec<Vec<u32>> = Vec::new();
            let mut signal_click: Option<(Vec<u32>, bool, u32)> = None;
            let mut row = 0usize;
            let roots = self.tree.roots.clone();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if roots.is_empty() {
                        ui.weak("Loading…");
                    }
                    let mut ctx = RenderCtx {
                        tree: &self.tree,
                        requested: &mut self.requested,
                        matrix_fetch: &mut self.matrix_fetch,
                        edits: &mut self.edits,
                        func_inputs: &mut self.func_inputs,
                        invocations: &mut self.invocations,
                        next_invocation_id: &mut self.next_invocation_id,
                        meter_range: &mut self.meter_range,
                        signal_click: &mut signal_click,
                        commands: &mut commands,
                        visible: &mut visible,
                        row: &mut row,
                    };
                    for root in &roots {
                        render_node(ui, &mut ctx, root);
                    }
                });

            // A matrix header was clicked → open that signal's parameters.
            if let Some((mpath, is_target, sig)) = signal_click {
                if let Some(node) = signal_param_path(&self.tree, &mpath, is_target, sig) {
                    if self.requested.insert(node.clone()) {
                        commands.push(NetCommand::GetDirectory(node.clone()));
                    }
                    let kind = if is_target { "target" } else { "source" };
                    self.signal_params = Some((node, format!("{kind} {sig}")));
                }
            }

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

        if let Some(id) = pending_open {
            self.open_provider(id);
        }
        self.signal_params_window(ui);
    }
}

/// Render an address-book node (folder or provider) in the left pane.
fn render_book_node(
    ui: &mut egui::Ui,
    node: &WireNode,
    current: Option<u64>,
    pending: &mut Option<u64>,
) {
    match node {
        WireNode::Folder { name, children } => {
            egui::CollapsingHeader::new(format!("📁 {name}"))
                .default_open(true)
                .show(ui, |ui| {
                    for c in children {
                        render_book_node(ui, c, current, pending);
                    }
                });
        }
        WireNode::Provider(p) => {
            if ui
                .selectable_label(current == Some(p.id), &p.name)
                .on_hover_text(format!("{}:{}", p.host, p.port))
                .clicked()
            {
                *pending = Some(p.id);
            }
        }
    }
}

/// Mutable render state threaded through the tree renderer.
struct RenderCtx<'a> {
    tree: &'a TreeModel,
    requested: &'a mut HashSet<Vec<u32>>,
    matrix_fetch: &'a mut HashSet<Vec<u32>>,
    edits: &'a mut HashMap<Vec<u32>, String>,
    func_inputs: &'a mut HashMap<(Vec<u32>, usize), String>,
    invocations: &'a mut HashMap<Vec<u32>, i32>,
    next_invocation_id: &'a mut i32,
    meter_range: &'a mut HashMap<Vec<u32>, (f64, f64)>,
    /// Set to `(matrix path, is_target, signal)` when a matrix header is clicked.
    signal_click: &'a mut Option<(Vec<u32>, bool, u32)>,
    commands: &'a mut Vec<NetCommand>,
    visible: &'a mut Vec<Vec<u32>>,
    /// Running parameter-row index, for alternating row striping.
    row: &'a mut usize,
}

impl WebApp {
    /// Popup of a matrix signal's parameters (gain/type/name), opened by clicking
    /// a row/column header in the grid.
    fn signal_params_window(&mut self, ui: &mut egui::Ui) {
        let Some((node_path, title)) = self.signal_params.clone() else {
            return;
        };
        let mut open = true;
        let mut commands: Vec<NetCommand> = Vec::new();
        egui::Window::new(format!("Signal · {title}"))
            .open(&mut open)
            .resizable(true)
            .default_width(280.0)
            .show(ui.ctx(), |ui| {
                let children = self
                    .tree
                    .get(&node_path)
                    .map(|n| n.children.clone())
                    .unwrap_or_default();
                if children.is_empty() {
                    ui.weak("loading…");
                    return;
                }
                egui::Grid::new(("sigparams", &node_path))
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for cp in &children {
                            if let Some(ce) = self.tree.get(cp) {
                                ui.label(egui::RichText::new(&ce.identifier).strong());
                                param_editor(ui, &self.tree, cp, &mut self.edits, &mut commands);
                                ui.end_row();
                            }
                        }
                    });
            });
        if !open {
            self.signal_params = None;
        }
        if let (Some(conn), Some(id)) = (&self.conn, self.current) {
            for cmd in &commands {
                conn.send_command(id, cmd);
            }
        }
    }
}

/// Resolve the parameter node path for a matrix signal (target/source).
fn signal_param_path(
    tree: &TreeModel,
    mpath: &[u32],
    is_target: bool,
    sig: u32,
) -> Option<Vec<u32>> {
    let m = tree.get(mpath)?.matrix.as_ref()?;
    let base = if is_target {
        m.param_targets_path.as_ref()?
    } else {
        m.param_sources_path.as_ref()?
    };
    let mut node = base.clone();
    node.push(sig);
    Some(node)
}

/// Render one tree entry (recursively). Pushes commands and records visible
/// parameter paths.
fn render_node(ui: &mut egui::Ui, ctx: &mut RenderCtx, path: &[u32]) {
    let tree = ctx.tree;
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
                    fetch_matrix(tree, path, m, ctx.matrix_fetch, ctx.commands);
                    if let Some((is_target, sig)) =
                        matrix_view::render_matrix(ui, entry, m, true, ctx.commands)
                    {
                        *ctx.signal_click = Some((path.to_vec(), is_target, sig));
                    }
                });
            return;
        }

        // Function: argument form + invoke + result (shared with the desktop).
        if let (Kind::Function, Some(f)) = (entry.kind, &entry.function) {
            let heading = if entry.is_online {
                egui::RichText::new(label)
            } else {
                egui::RichText::new(format!("{label} (offline)")).weak()
            };
            egui::CollapsingHeader::new(heading)
                .id_salt(path)
                .show(ui, |ui| {
                    widgets::render_function(
                        ui,
                        entry,
                        f,
                        ctx.func_inputs,
                        ctx.invocations,
                        ctx.next_invocation_id,
                        &tree.invocation_results,
                        ctx.commands,
                    );
                });
            return;
        }

        // A plain node: expand to fetch and show children.
        let heading = if entry.is_online {
            egui::RichText::new(label)
        } else {
            egui::RichText::new(format!("{label} (offline)")).weak()
        };
        let header = egui::CollapsingHeader::new(heading)
            .id_salt(path)
            .show(ui, |ui| {
                let children = entry.children.clone();
                if children.is_empty() {
                    ui.weak("…");
                }
                for child in &children {
                    render_node(ui, ctx, child);
                }
            });
        if header.openness > 0.0 && ctx.requested.insert(path.to_vec()) {
            ctx.commands.push(NetCommand::GetDirectory(path.to_vec()));
        }
        return;
    }

    // A leaf parameter.
    ctx.visible.push(path.to_vec());
    let row = *ctx.row;
    *ctx.row += 1;
    // Reserve a background shape so the stripe paints behind the whole row.
    let bg = ui.painter().add(egui::Shape::Noop);
    let resp = ui
        .horizontal(|ui| {
            let (badge, color) = if entry.is_writable() {
                ("rw", egui::Color32::from_rgb(70, 130, 200))
            } else {
                ("ro", egui::Color32::from_gray(140))
            };
            ui.label(egui::RichText::new(badge).monospace().small().color(color));
            ui.label(egui::RichText::new(entry.label()).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                // Inline live meter for numeric parameters (fixed width so the
                // row doesn't jiggle as the value's digit count changes).
                if widgets::is_meterable(entry) {
                    let range = widgets::meter_range(entry, ctx.meter_range);
                    let value = entry.value.as_ref().and_then(widgets::value_f64);
                    let h = ui.spacing().interact_size.y;
                    widgets::draw_vmeter(ui, value, range, 10.0, h);
                }
                param_editor(ui, tree, path, ctx.edits, ctx.commands);
            });
        })
        .response;

    // Faint alternating stripe behind odd rows.
    if row % 2 == 1 {
        let stripe = if ui.visuals().dark_mode {
            egui::Color32::from_white_alpha(8)
        } else {
            egui::Color32::from_black_alpha(8)
        };
        let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), resp.rect.y_range());
        ui.painter()
            .set(bg, egui::Shape::rect_filled(rect, 0.0, stripe));
    }
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
    // Fixed-width read-only numeric display (so the row doesn't jiggle as the
    // value's digit count changes), showing the scaled/formatted value.
    let ro_num = |ui: &mut egui::Ui, text: String| {
        ui.add_sized(
            egui::vec2(64.0, ui.spacing().interact_size.y),
            egui::Label::new(text),
        );
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
                ro_num(ui, widgets::display_value(entry, &Value::Integer(*i)));
            }
        }
        Some(Value::Real(r)) => {
            if writable {
                let mut v = r.to_f64();
                if ui.add(egui::DragValue::new(&mut v).speed(0.1)).changed() {
                    set(commands, Value::Real(v.into()));
                }
            } else {
                ro_num(ui, widgets::display_value(entry, &Value::Real(r.clone())));
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
