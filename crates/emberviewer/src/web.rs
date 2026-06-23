//! The browser client: a lean egui UI that mirrors one provider at a time over
//! the WebSocket. It reuses the shared [`TreeModel`] and command vocabulary; the
//! heavier desktop chrome (address book, matrices, meters) is desktop-only for
//! now - this focuses on browsing the tree and viewing/setting values.
#![allow(deprecated)] // egui Panel aliases, as in `app` (migrate when settled)

use std::collections::{HashMap, HashSet};

use ember_proto::glow::{self, Value};
use ember_web_proto::{ClientMsg, WireNode, WireProvider, WireStatus};

use crate::matrix_view;
use crate::model::{
    format_value, label_fetch_step, Kind, MatrixInfo, TreeModel, LABEL_FETCH_RETRY_SECS,
};
use crate::net::NetCommand;
use crate::web_transport::{WebEvent, WsConnection};
use crate::widgets;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(217, 119, 43);
/// Amber marking an optimistic (locally-set, unconfirmed) parameter value.
const PENDING_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 160, 30);

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
    /// Expanded nodes: path -> (last fetch time, attempts). Re-fetches an
    /// open-but-empty node a few times so a dropped response self-heals.
    requested: HashMap<Vec<u32>, (f64, u8)>,
    /// Matrix-related sub-trees: path -> (last request time, attempts). The
    /// matrix dir is asked once; label sub-tree nodes are re-asked (throttled,
    /// capped) until they report children, since embedded devices drop
    /// getDirectory requests during the initial discovery burst.
    matrix_fetch: HashMap<Vec<u32>, (f64, u8)>,
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
    /// Selected parameter (shown large in the right meter pane).
    selected: Option<Vec<u32>>,
    /// Open "signal parameters" popup (matrix header click): node path + title.
    signal_params: Option<(Vec<u32>, String)>,
    /// Open multi-line string editor/viewer, if any.
    string_edit: Option<widgets::StringEdit>,
    /// Paths set optimistically and not yet confirmed by a provider value update,
    /// shown as "pending" so an unconfirmed value reads differently from a real one.
    pending: HashSet<Vec<u32>>,
    /// Last "denied" message from the server (read-only mode).
    denied: Option<String>,
    dark: bool,
    /// Per-browser safety: locked (the default) greys the value/route/invoke
    /// controls until the user taps the padlock to arm them. Resets to locked on
    /// every page load.
    locked: bool,
    /// egui time until which the padlock flashes after a blocked-while-locked tap.
    flash_until: f64,
    /// egui time of the last reconnect attempt (for throttling).
    last_reconnect: f64,
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
            requested: HashMap::new(),
            matrix_fetch: HashMap::new(),
            subscribed: HashSet::new(),
            edits: HashMap::new(),
            func_inputs: HashMap::new(),
            invocations: HashMap::new(),
            next_invocation_id: 1,
            meter_range: HashMap::new(),
            selected: None,
            signal_params: None,
            string_edit: None,
            pending: HashSet::new(),
            denied: None,
            dark: true,
            locked: true,
            flash_until: 0.0,
            last_reconnect: 0.0,
        }
    }

    /// Attempt to (re)open the WebSocket - used at startup and to auto-reconnect
    /// after the server goes away (e.g. the desktop app was relaunched).
    fn try_reconnect(&mut self, ctx: &egui::Context, now: f64) {
        if now - self.last_reconnect < 2.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
            return;
        }
        self.last_reconnect = now;
        if let Some(url) = ws_url() {
            if let Some(c) = WsConnection::connect(&url, ctx.clone()) {
                c.send(ClientMsg::Auth {
                    token: url_query_param("token"),
                });
                self.conn = Some(c);
                self.closed = false;
            }
        }
        // Keep waking so retries continue even when the UI is idle.
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }

    fn open_provider(&mut self, id: u64) {
        self.current = Some(id);
        self.tree = TreeModel::new();
        self.requested.clear();
        self.matrix_fetch.clear();
        self.subscribed.clear();
        self.invocations.clear();
        self.meter_range.clear();
        self.selected = None;
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
                    self.closed = false;
                    // After a reconnect, re-open whatever we were viewing.
                    if let Some(id) = self.current {
                        self.open_provider(id);
                    }
                }
                WebEvent::AuthRejected => {
                    self.auth_error = Some("Access denied - check the token in the URL.".into());
                }
                WebEvent::Providers(list) => self.providers = list,
                WebEvent::AddressBook(nodes) => self.address_tree = nodes,
                WebEvent::Status { id, status } => {
                    if self.current == Some(id) {
                        // A `Connecting` status means the shared connection is moving
                        // to a new address - drop the old device's tree and await
                        // fresh documents. Keep `subscribed`: the hub replays
                        // subscriptions to the new endpoint, so ref-counts stay valid.
                        if matches!(status, WireStatus::Connecting) {
                            self.tree = TreeModel::new();
                            self.requested.clear();
                            self.matrix_fetch.clear();
                            self.invocations.clear();
                            self.meter_range.clear();
                            self.selected = None;
                            self.signal_params = None;
                            self.pending.clear();
                        }
                        self.status = Some(status);
                    }
                }
                WebEvent::Document { id, root } => {
                    if self.current == Some(id) {
                        self.tree.merge(root);
                        // A value update from the provider is authoritative: clear
                        // the optimistic "pending" mark on those paths.
                        for p in self.tree.take_value_updates() {
                            self.pending.remove(&p);
                        }
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

        // Auto-reconnect if the socket went away (e.g. the desktop app restarted).
        let now = ui.input(|i| i.time);
        if (self.conn.is_none() || self.closed) && self.auth_error.is_none() {
            self.try_reconnect(ui.ctx(), now);
        }

        let mut pending_open: Option<u64> = None;

        egui::TopBottomPanel::top("webbar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("emberviewer").color(ACCENT));
                ui.separator();
                if let Some(st) = &self.status {
                    ui.label(status_text(st));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Safety padlock: locked by default, so a stray tap can't change
                    // a live device. Tap to arm; tap again to lock.
                    widgets::lock_toggle(ui, &mut self.locked, now, self.flash_until);
                    if ui
                        .button(if self.dark { "Light" } else { "Dark" })
                        .on_hover_text("Toggle theme")
                        .clicked()
                    {
                        self.dark = !self.dark;
                    }
                    ui.separator();
                    // Project links. The GitHub Octocat (special_emojis::GITHUB)
                    // and the globe (U+1F310) are both in egui's bundled fonts -
                    // verified to render, unlike most symbol glyphs here.
                    icon_link(
                        ui,
                        '\u{1F310}',
                        "https://mattlamb99.github.io/emberviewer",
                        "Website / docs",
                    );
                    icon_link(
                        ui,
                        egui::special_emojis::GITHUB,
                        "https://github.com/mattlamb99/emberviewer",
                        "GitHub repository",
                    );
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

        // Right pane: a large meter for the selected parameter.
        if self.authed {
            if let Some(entry) = self
                .selected
                .clone()
                .and_then(|p| self.tree.get(&p).cloned())
                .filter(widgets::is_meterable)
            {
                egui::SidePanel::right("meterpane")
                    .resizable(false)
                    .exact_width(112.0)
                    .show_inside(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(entry.label()).small().strong());
                            let range = widgets::meter_range(&entry, &mut self.meter_range);
                            let value = entry.value.as_ref().and_then(widgets::value_f64);
                            let h = (ui.available_height() - 26.0).max(60.0);
                            widgets::draw_vmeter(ui, value, range, 38.0, h);
                            if let Some(v) = value {
                                ui.label(
                                    egui::RichText::new(widgets::meter_readout(&entry, v)).small(),
                                );
                            }
                        });
                    });
            }
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
                ui.colored_label(egui::Color32::from_rgb(210, 150, 40), "Reconnecting…");
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
                        now,
                        requested: &mut self.requested,
                        matrix_fetch: &mut self.matrix_fetch,
                        edits: &mut self.edits,
                        string_edit: &mut self.string_edit,
                        pending: &self.pending,
                        func_inputs: &mut self.func_inputs,
                        invocations: &mut self.invocations,
                        next_invocation_id: &mut self.next_invocation_id,
                        meter_range: &mut self.meter_range,
                        signal_click: &mut signal_click,
                        commands: &mut commands,
                        visible: &mut visible,
                        row: &mut row,
                        selected: &mut self.selected,
                        armed: !self.locked,
                        flash: &mut self.flash_until,
                    };
                    for root in &roots {
                        render_node(ui, &mut ctx, root);
                    }
                });

            // A matrix header was clicked → open that signal's parameters.
            if let Some((mpath, is_target, sig)) = signal_click {
                if let Some(node) = signal_param_path(&self.tree, &mpath, is_target, sig) {
                    if !self.requested.contains_key(&node) {
                        self.requested.insert(node.clone(), (now, 1));
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
        self.string_edit_window(ui);
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
    /// egui time, for throttled re-fetch of empty nodes.
    now: f64,
    requested: &'a mut HashMap<Vec<u32>, (f64, u8)>,
    matrix_fetch: &'a mut HashMap<Vec<u32>, (f64, u8)>,
    edits: &'a mut HashMap<Vec<u32>, String>,
    /// Open multi-line string editor/viewer, set when a row's `Edit…`/`View…`
    /// button is clicked.
    string_edit: &'a mut Option<widgets::StringEdit>,
    /// Paths with an unconfirmed optimistic value (rendered as "pending").
    pending: &'a HashSet<Vec<u32>>,
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
    /// Currently-selected parameter (shown in the right meter pane).
    selected: &'a mut Option<Vec<u32>>,
    /// Whether value/route/invoke controls are interactive (safety toggle).
    armed: bool,
    /// Set to `now + LOCK_FLASH_SECS` when a locked control is tapped, to flash
    /// the padlock and explain why nothing happened.
    flash: &'a mut f64,
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
        let armed = !self.locked;
        let now = ui.input(|i| i.time);
        let mut flashed = false;
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
                                let pending = self.pending.contains(cp);
                                let (_, blocked) = widgets::lockable(ui, armed, |ui| {
                                    param_editor(
                                        ui,
                                        &self.tree,
                                        cp,
                                        &mut self.edits,
                                        &mut self.string_edit,
                                        pending,
                                        &mut commands,
                                    );
                                });
                                flashed |= blocked;
                                ui.end_row();
                            }
                        }
                    });
            });
        if !open {
            self.signal_params = None;
        }
        if flashed {
            self.flash_until = now + widgets::LOCK_FLASH_SECS;
        }
        if let (Some(conn), Some(id)) = (&self.conn, self.current) {
            for cmd in &commands {
                conn.send_command(id, cmd);
            }
        }
    }

    /// Multi-line string editor/viewer window (opened from a string parameter's
    /// `Edit…`/`View…` button); applies edits via `SetValue`.
    fn string_edit_window(&mut self, ui: &mut egui::Ui) {
        if let Some((path, value)) = widgets::string_edit_window(ui.ctx(), &mut self.string_edit) {
            // Optimistically reflect the applied value: many SDP/string params are
            // write-mostly and never echo the value back, which would otherwise
            // leave the row blank after Apply. A real device update still wins.
            if let Some(e) = self.tree.entries.get_mut(&path) {
                e.value = Some(value.clone());
            }
            // Mark optimistic until the provider's value update confirms it.
            self.pending.insert(path.clone());
            if let (Some(conn), Some(id)) = (&self.conn, self.current) {
                conn.send_command(id, &NetCommand::SetValue(path.clone(), value));
                // Re-read so the device's authoritative value wins (e.g. it
                // rejected a malformed SDP and blanked its own value).
                conn.send_command(id, &NetCommand::RefreshValue(path));
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
                    // Keep ticking while a label fetch is still outstanding so the
                    // throttled retry fires even when nothing else repaints.
                    if fetch_matrix(tree, path, m, ctx.matrix_fetch, ctx.now, ctx.commands) {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_secs_f64(
                                LABEL_FETCH_RETRY_SECS,
                            ));
                    }
                    let (clicked, blocked) = widgets::lockable(ui, ctx.armed, |ui| {
                        matrix_view::render_matrix(ui, entry, m, true, ctx.commands)
                    });
                    if blocked {
                        *ctx.flash = ctx.now + widgets::LOCK_FLASH_SECS;
                    }
                    if let Some((is_target, sig)) = clicked {
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
                    let (_, blocked) = widgets::lockable(ui, ctx.armed, |ui| {
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
                    if blocked {
                        *ctx.flash = ctx.now + widgets::LOCK_FLASH_SECS;
                    }
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
        if header.openness > 0.0 {
            // Fetch on first open; re-fetch a few times if still empty (so a
            // dropped getDirectory response self-heals).
            let due = match ctx.requested.get(path) {
                None => true,
                Some(&(t, n)) => entry.children.is_empty() && n < 3 && (ctx.now - t) > 3.0,
            };
            if due {
                let n = ctx.requested.get(path).map(|&(_, n)| n).unwrap_or(0);
                ctx.requested.insert(path.to_vec(), (ctx.now, n + 1));
                ctx.commands.push(NetCommand::GetDirectory(path.to_vec()));
            }
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
            if ctx.pending.contains(path) {
                // Amber dot: value set locally, not yet confirmed by the provider.
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(10.0, ui.spacing().interact_size.y),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .circle_filled(rect.center(), 4.0, PENDING_COLOR);
                resp.on_hover_text("Pending: set locally, awaiting device confirmation");
            }
            let (badge, color) = if entry.is_writable() {
                ("rw", egui::Color32::from_rgb(70, 130, 200))
            } else {
                ("ro", egui::Color32::from_gray(140))
            };
            ui.label(egui::RichText::new(badge).monospace().small().color(color));
            // Clicking the name selects the parameter (drives the meter pane).
            let is_sel = ctx.selected.as_deref() == Some(path);
            if ui
                .selectable_label(is_sel, egui::RichText::new(entry.label()).strong())
                .clicked()
            {
                *ctx.selected = Some(path.to_vec());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                // Inline live meter for numeric parameters (fixed width so the
                // row doesn't jiggle as the value's digit count changes); click to
                // pin it large in the right pane.
                if widgets::is_meterable(entry) {
                    let range = widgets::meter_range(entry, ctx.meter_range);
                    let value = entry.value.as_ref().and_then(widgets::value_f64);
                    let h = ui.spacing().interact_size.y;
                    if widgets::draw_vmeter(ui, value, range, 10.0, h).clicked() {
                        *ctx.selected = Some(path.to_vec());
                    }
                }
                let pending = ctx.pending.contains(path);
                let (_, blocked) = widgets::lockable(ui, ctx.armed, |ui| {
                    param_editor(
                        ui,
                        tree,
                        path,
                        ctx.edits,
                        ctx.string_edit,
                        pending,
                        ctx.commands,
                    );
                });
                if blocked {
                    *ctx.flash = ctx.now + widgets::LOCK_FLASH_SECS;
                }
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
    string_edit: &mut Option<widgets::StringEdit>,
    pending: bool,
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
            // Pop-out multi-line editor/viewer for long/multi-line values (SDPs).
            let (label, hover) = if writable {
                ("Edit…", "Edit in a larger box")
            } else {
                ("View…", "View in a larger box")
            };
            if ui.button(label).on_hover_text(hover).clicked() {
                *string_edit = Some(widgets::StringEdit::new(entry, s));
            }
            if writable && !s.contains(['\n', '\r']) {
                let buf = edits.entry(path.to_vec()).or_insert_with(|| s.clone());
                let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(160.0));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let v = buf.clone();
                    set(commands, Value::String(v));
                }
            } else {
                // Keep line breaks (CR dropped) and wrap so a multi-line value
                // grows the row taller; multi-line writable is edited via the
                // pop-out, single-line read-only just shows.
                let mut text = egui::RichText::new(widgets::clean_multiline(s));
                if pending {
                    text = text.color(PENDING_COLOR);
                }
                ui.add(egui::Label::new(text).wrap());
            }
        }
        Some(v) => {
            ui.label(format_value(v));
        }
        None => {
            ui.weak("-");
        }
    }
}

/// A clickable icon glyph in the top bar that opens `url` in a new tab.
fn icon_link(ui: &mut egui::Ui, glyph: char, url: &str, hover: &str) {
    let text = egui::RichText::new(glyph).size(18.0).color(ACCENT);
    ui.hyperlink_to(text, url).on_hover_text(hover);
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
/// sources / connections, asked once) and its label / parameter sub-trees.
/// Returns true while any label sub-tree node is still unsatisfied (so the
/// caller keeps repainting until the throttled retry fires).
fn fetch_matrix(
    tree: &TreeModel,
    path: &[u32],
    m: &MatrixInfo,
    mfetch: &mut HashMap<Vec<u32>, (f64, u8)>,
    now: f64,
    commands: &mut Vec<NetCommand>,
) -> bool {
    // The matrix's own directory is requested once.
    if !mfetch.contains_key(path) {
        mfetch.insert(path.to_vec(), (now, 1));
        commands.push(NetCommand::GetMatrixDirectory(path.to_vec()));
    }
    let mut pending = false;
    for base in &m.label_paths {
        // basePath may be absolute or relative-to-parent depending on the
        // provider; fetch each interpretation that points at a real node.
        for cand in crate::model::fetchable_label_bases(tree, path, base) {
            pending |= fetch_subtree(tree, &cand, mfetch, now, commands);
        }
    }
    if let Some(ploc) = &m.params_location {
        pending |= fetch_subtree(tree, ploc, mfetch, now, commands);
    }
    pending
}

/// Fetch a sub-tree base and (once known) its immediate children, re-requesting
/// any still-childless node (throttled, capped). Returns true while unsatisfied.
fn fetch_subtree(
    tree: &TreeModel,
    base: &[u32],
    mfetch: &mut HashMap<Vec<u32>, (f64, u8)>,
    now: f64,
    commands: &mut Vec<NetCommand>,
) -> bool {
    let mut pending = fetch_if_empty(tree, base, mfetch, now, commands);
    if let Some(e) = tree.get(base) {
        for child in &e.children {
            pending |= fetch_if_empty(tree, child, mfetch, now, commands);
        }
    }
    pending
}

/// Request `path`'s directory if it has no children yet and retries remain.
/// Returns true while the node is still unsatisfied and within its retry budget.
fn fetch_if_empty(
    tree: &TreeModel,
    path: &[u32],
    mfetch: &mut HashMap<Vec<u32>, (f64, u8)>,
    now: f64,
    commands: &mut Vec<NetCommand>,
) -> bool {
    let has_children = tree.get(path).is_some_and(|e| !e.children.is_empty());
    let step = label_fetch_step(mfetch.get(path).copied(), has_children, now);
    if let Some(new_state) = step.new_state {
        mfetch.insert(path.to_vec(), new_state);
    }
    if step.request {
        commands.push(NetCommand::GetDirectory(path.to_vec()));
    }
    step.pending
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
