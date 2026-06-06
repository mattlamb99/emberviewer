//! The eframe application: address-book sidebar + provider tree browser.
//!
//! egui 0.34 deprecated the `SidePanel`/`TopBottomPanel` aliases in favour of a
//! unified `Panel` API; the aliases still work, so we keep them for now and
//! migrate when that API settles.
#![allow(deprecated)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ember_proto::glow::{self, Value};
use ember_web_proto::WireProvider;

use crate::address_book::{AddressBook, Id, Node, DEFAULT_PORT};
use crate::hub::HubRegistry;
use crate::model::{format_value, TreeModel};
use crate::net::{ConnectionHandle, NetCommand, NetEvent};
use crate::server::{self, ServerHandle};
use crate::settings::{OrderBy, Settings, StartupMode};
use crate::widgets::{draw_vmeter, is_meterable, meter_range, render_function, value_f64};

/// State for one open provider connection.
struct Session {
    addr: String,
    name: String,
    handle: ConnectionHandle,
    tree: TreeModel,
    status: Status,
    /// UI expansion state per path.
    open: HashMap<Vec<u32>, bool>,
    /// In-progress value edits, keyed by path.
    edits: HashMap<Vec<u32>, String>,
    /// Parameters we currently hold a value-change subscription for.
    subscribed: HashSet<Vec<u32>>,
    /// Tree filter text (matches identifiers; empty = show all).
    filter: String,
    /// Pending boolean "pulse" resets: path -> when to send `false`.
    pulses: HashMap<Vec<u32>, std::time::Instant>,
    /// Function argument input buffers: (function path, arg index) -> text.
    func_inputs: HashMap<(Vec<u32>, usize), String>,
    /// Last invocation id issued per function path (to look up its result).
    invocations: HashMap<Vec<u32>, i32>,
    /// Monotonic invocation id source.
    next_invocation_id: i32,
    /// Parameter paths whose value changes are being logged.
    logged: HashSet<Vec<u32>>,
    /// Currently selected parameter (shown in the right meter panel).
    selected: Option<Vec<u32>>,
    /// Popped-out meter windows for this session.
    popped: Vec<PoppedMeter>,
    /// Auto-tracked value range per meter path (for params without min/max).
    meter_range: HashMap<Vec<u32>, (f64, f64)>,
    /// Label sub-tree paths already requested (dedup for eager matrix-label fetch).
    label_fetch: HashSet<Vec<u32>>,
    /// Open "signal parameters" popup: the signal's parameter node path + a title.
    signal_params: Option<(Vec<u32>, String)>,
}

/// A meter popped into its own always-pinnable window.
struct PoppedMeter {
    path: Vec<u32>,
    always_on_top: bool,
}

/// One line in the change log.
#[derive(Clone)]
struct LogEntry {
    time: String,
    provider: String,
    label: String,
    path: String,
    value: String,
}

#[derive(Clone, PartialEq)]
enum Status {
    Connecting,
    Connected,
    Reconnecting { secs: u64, reason: String },
    Disconnected(String),
}

/// Draft state for the add/edit provider dialog.
#[derive(Default)]
struct AddDialog {
    open: bool,
    name: String,
    host: String,
    port: String,
    parent: Id,
    /// `Some(id)` when editing an existing provider; `None` when adding.
    editing: Option<Id>,
}

/// Drag-and-drop payload: the id of the node being dragged.
#[derive(Clone)]
struct DragPayload(Id);

/// An action requested from the sidebar (right-click menu or drag-drop).
enum SidebarAction {
    Open(Id),
    Disconnect(Id),
    EditProvider(Id),
    Remove(Id),
    AddFolder(Id),
    RenameFolder(Id),
    Move { node: Id, into: Id },
}

/// Draft state for the create/rename folder dialog.
#[derive(Default)]
struct FolderDialog {
    open: bool,
    name: String,
    parent: Id,
    /// `Some(id)` when renaming an existing folder.
    rename: Option<Id>,
}

/// Settings that affect how the provider tree is rendered, bundled so they can
/// be threaded through the recursive render functions.
struct RenderOpts {
    pulse_ms: u64,
    show_descriptions: bool,
    order_by: OrderBy,
    matrix_targets_on_top: bool,
}

pub struct App {
    rt: tokio::runtime::Runtime,
    book: AddressBook,
    /// One session per connected provider id.
    sessions: HashMap<Id, Session>,
    /// Currently selected provider (drives the main panel).
    active: Option<Id>,
    add: AddDialog,
    folder_dialog: FolderDialog,
    status_line: String,
    /// Filter text for the providers sidebar.
    provider_filter: String,
    settings: Settings,
    show_options: bool,
    /// Change-log buffer (newest last).
    log: Vec<LogEntry>,
    show_log: bool,
    /// Active mDNS discovery, if running.
    discovery: Option<crate::discovery::Discovery>,
    show_discovery: bool,
    show_about: bool,
    /// Theme currently applied to the egui context (`None` until the first frame
    /// applies it). Re-applied whenever it differs from `settings.dark_mode`.
    applied_dark: Option<bool>,
    /// Shared per-provider connections (desktop + web viewers).
    hubs: HubRegistry,
    /// Provider list exposed to the web server (kept in sync with the book).
    catalog: server::Catalog,
    /// Running web server, when server mode is enabled.
    server: Option<ServerHandle>,
}

/// The project's warm-orange brand accent.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(217, 119, 43);

/// Apply the chosen base theme (dark/light) and overlay a hint of the brand
/// accent. Used both at startup and when the theme toggle changes.
fn apply_theme(ctx: &egui::Context, dark: bool) {
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    // Selection picks up a muted accent (also drives the matrix crosspoint
    // highlight and the row-selection tint, which derive from this colour).
    visuals.selection.bg_fill = ACCENT.gamma_multiply(if dark { 0.55 } else { 0.40 });
    visuals.selection.stroke.color = ACCENT;
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.hovered.bg_stroke.color = ACCENT.gamma_multiply(0.6);
    ctx.set_visuals(visuals);
}

/// A random hex access token for server mode.
fn generate_token() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let book = AddressBook::load().unwrap_or_default();
        let settings = Settings::load();
        let hubs = HubRegistry::new(rt.handle().clone(), cc.egui_ctx.clone());
        let mut app = App {
            rt,
            book,
            sessions: HashMap::new(),
            active: None,
            add: AddDialog::default(),
            folder_dialog: FolderDialog::default(),
            status_line: String::new(),
            provider_filter: String::new(),
            settings,
            show_options: false,
            log: Vec::new(),
            show_log: false,
            discovery: None,
            show_discovery: false,
            show_about: false,
            applied_dark: None,
            hubs,
            catalog: Arc::new(Mutex::new(Vec::new())),
            server: None,
        };
        // The theme is applied from within `ui()` (eframe overrides visuals set
        // here during construction, which is why the startup theme didn't stick).
        app.apply_startup_mode(&cc.egui_ctx.clone());
        app.sync_server(); // resume server mode if it was enabled at last shutdown
        app
    }

    /// Auto-connect providers per the configured startup mode.
    fn apply_startup_mode(&mut self, ctx: &egui::Context) {
        let ids: Vec<Id> = match self.settings.startup_mode {
            StartupMode::ConnectNone => Vec::new(),
            StartupMode::ConnectAll => self
                .book
                .iter()
                .filter_map(|(_, n)| match n {
                    Node::Provider(p) => Some(p.id),
                    _ => None,
                })
                .collect(),
            StartupMode::ConnectLast => self.settings.last_connected.clone(),
        };
        for id in ids {
            if self.book.find_provider(id).is_some() {
                self.connect(id, ctx);
            }
        }
        self.active = self.sessions.keys().copied().min();
    }

    /// Record currently-open providers as the "last session" and persist.
    fn remember_session(&mut self) {
        let mut ids: Vec<Id> = self.sessions.keys().copied().collect();
        ids.sort_unstable();
        if ids != self.settings.last_connected {
            self.settings.last_connected = ids;
            let _ = self.settings.save();
        }
    }

    /// Switch to an already-open session, or connect if not yet open.
    fn open_provider(&mut self, id: Id, ctx: &egui::Context) {
        if self.sessions.contains_key(&id) {
            self.active = Some(id);
        } else {
            self.connect(id, ctx);
        }
    }

    /// Close a session: drop the desktop's view. The shared connection stays up
    /// only if a web client is still viewing it.
    fn disconnect(&mut self, id: Id) {
        self.sessions.remove(&id);
        if self.active == Some(id) {
            self.active = self.sessions.keys().copied().next();
        }
        self.remember_session();
    }

    fn connect(&mut self, id: Id, _ctx: &egui::Context) {
        let Some(provider) = self.book.find_provider(id).cloned() else {
            return;
        };
        let addr = format!("{}:{}", provider.host, provider.port);
        let handle =
            ConnectionHandle::open(&self.hubs, id, addr.clone(), self.settings.send_keepalive);
        self.sessions.insert(
            id,
            Session {
                addr,
                name: provider.name.clone(),
                handle,
                tree: TreeModel::new(),
                status: Status::Connecting,
                open: HashMap::new(),
                edits: HashMap::new(),
                subscribed: HashSet::new(),
                filter: String::new(),
                pulses: HashMap::new(),
                func_inputs: HashMap::new(),
                invocations: HashMap::new(),
                next_invocation_id: 1,
                logged: HashSet::new(),
                selected: None,
                popped: Vec::new(),
                meter_range: HashMap::new(),
                label_fetch: HashSet::new(),
                signal_params: None,
            },
        );
        self.active = Some(id);
        self.remember_session();
    }

    /// Rebuild the provider list the web server exposes from the address book.
    fn refresh_catalog(&self) {
        let list: Vec<WireProvider> = self
            .book
            .iter()
            .filter_map(|(_, n)| match n {
                Node::Provider(p) => Some(WireProvider {
                    id: p.id,
                    name: p.name.clone(),
                    host: p.host.clone(),
                    port: p.port,
                }),
                Node::Folder(_) => None,
            })
            .collect();
        *self.catalog.lock().unwrap() = list;
    }

    /// Start or stop the web server to match `settings.server_enabled`.
    fn sync_server(&mut self) {
        match (self.settings.server_enabled, self.server.is_some()) {
            (true, false) => {
                if self.settings.server_token.trim().is_empty() && !self.settings.server_open_lan {
                    self.settings.server_token = generate_token();
                    let _ = self.settings.save();
                }
                self.refresh_catalog();
                let cfg = server::ServerConfig {
                    port: self.settings.server_port,
                    token: self.settings.server_token.clone(),
                    open_lan: self.settings.server_open_lan,
                    read_only: self.settings.server_read_only,
                    keepalive: self.settings.send_keepalive,
                };
                match server::start(
                    self.rt.handle(),
                    self.hubs.clone(),
                    self.catalog.clone(),
                    cfg,
                ) {
                    Ok(h) => {
                        self.status_line = format!("server listening on port {}", h.bound.port());
                        self.server = Some(h);
                    }
                    Err(e) => {
                        self.status_line = format!("server failed to start: {e}");
                        self.settings.server_enabled = false;
                        let _ = self.settings.save();
                    }
                }
            }
            (false, true) => {
                self.server = None; // drop → graceful shutdown
                self.status_line = "server stopped".into();
            }
            _ => {}
        }
    }

    /// Restart the server (after a config change) if it is running.
    fn restart_server(&mut self) {
        if self.server.is_some() {
            self.server = None;
            self.sync_server();
        }
    }

    /// Drain network events for every session and apply them to the model.
    fn pump_network(&mut self) {
        let clear_on_disconnect = self.settings.clear_tree_on_disconnect;
        let mut new_logs: Vec<LogEntry> = Vec::new();
        for session in self.sessions.values_mut() {
            let provider = session.name.clone();
            for event in session.handle.drain() {
                match event {
                    NetEvent::Connected => {
                        session.status = Status::Connected;
                        // Re-fetch every expanded node on the new connection. The
                        // hub re-subscribes active paths itself, so we keep our
                        // `subscribed` set intact (clearing it would desync the
                        // ref-counts and leave orphaned device subscriptions).
                        for e in session.tree.entries.values_mut() {
                            e.requested = false;
                        }
                    }
                    NetEvent::Document(root) => {
                        // Snapshot logged params' values, merge, then log changes.
                        let snaps: Vec<(Vec<u32>, Option<Value>)> = session
                            .logged
                            .iter()
                            .map(|p| (p.clone(), session.tree.get(p).and_then(|e| e.value.clone())))
                            .collect();
                        session.tree.merge(root);
                        for (p, old) in snaps {
                            if let Some(e) = session.tree.get(&p) {
                                if e.value.is_some() && e.value != old {
                                    new_logs.push(LogEntry {
                                        time: timestamp(),
                                        provider: provider.clone(),
                                        label: e.label(),
                                        path: path_string(&p),
                                        value: e
                                            .value
                                            .as_ref()
                                            .map(format_value)
                                            .unwrap_or_default(),
                                    });
                                }
                            }
                        }
                    }
                    NetEvent::Reconnecting {
                        retry_in_secs,
                        reason,
                    } => {
                        session.status = Status::Reconnecting {
                            secs: retry_in_secs,
                            reason,
                        };
                    }
                    NetEvent::Disconnected(reason) => {
                        session.status =
                            Status::Disconnected(reason.unwrap_or_else(|| "closed".into()));
                        if clear_on_disconnect {
                            session.tree = TreeModel::new();
                            session.subscribed.clear();
                        }
                    }
                    NetEvent::Error(e) => self.status_line = format!("error: {e}"),
                }
            }
        }
        for entry in new_logs {
            self.push_log(entry);
        }
    }

    /// Append a log entry to the buffer and (if configured) the log file.
    fn push_log(&mut self, entry: LogEntry) {
        let path = self.settings.log_file.trim();
        if !path.is_empty() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(
                    f,
                    "{} [{}] {} ({}) = {}",
                    entry.time, entry.provider, entry.label, entry.path, entry.value
                );
            }
        }
        self.log.push(entry);
        let len = self.log.len();
        if len > 5000 {
            self.log.drain(0..len - 5000);
        }
    }
}

/// Current wall-clock time as `HH:MM:SS` (UTC).
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Apply the persisted theme on the first frame (and whenever it changes);
        // doing this in `ui()` rather than construction is what makes it stick.
        if self.applied_dark != Some(self.settings.dark_mode) {
            apply_theme(&ctx, self.settings.dark_mode);
            self.applied_dark = Some(self.settings.dark_mode);
        }
        // Keep the web server's provider list current with the address book.
        if self.server.is_some() {
            self.refresh_catalog();
        }
        self.pump_network();
        self.process_pulses(&ctx);
        self.poll_discovery(&ctx);
        self.popped_meters(&ctx);

        egui::TopBottomPanel::top("menubar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Options").clicked() {
                    self.show_options = true;
                }
                if ui
                    .selectable_label(self.show_log, "Log")
                    .on_hover_text("Show the change log")
                    .clicked()
                {
                    self.show_log = !self.show_log;
                }
                if ui
                    .selectable_label(self.show_discovery, "Discover")
                    .on_hover_text("Find Ember+ providers on the network (mDNS)")
                    .clicked()
                {
                    self.toggle_discovery();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(self.show_about, "About").clicked() {
                        self.show_about = !self.show_about;
                    }
                });
            });
        });

        self.sidebar(ui, &ctx);
        self.add_dialog(&ctx);
        self.folder_dialog(&ctx);
        self.options_window(&ctx);
        self.about_window(&ctx);
        self.signal_params_window(&ctx);
        self.discovery_window(&ctx);
        self.tabs(ui);

        egui::TopBottomPanel::bottom("status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(id) = self.active {
                    if let Some(s) = self.sessions.get(&id) {
                        let txt = match &s.status {
                            Status::Connecting => "connecting…".to_string(),
                            Status::Connected => format!("connected · {}", s.addr),
                            Status::Reconnecting { secs, reason } => {
                                format!("reconnecting in {secs}s · {reason}")
                            }
                            Status::Disconnected(r) => format!("disconnected · {r}"),
                        };
                        paint_dot(ui, status_color(Some(&s.status)));
                        ui.label(txt);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.status_line);
                });
            });
        });

        if self.show_log {
            egui::TopBottomPanel::bottom("logpanel")
                .resizable(true)
                .default_height(160.0)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong("Change log");
                        let logging_to_file = !self.settings.log_file.trim().is_empty();
                        if logging_to_file {
                            ui.weak(format!("→ {}", self.settings.log_file.trim()));
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.button("Clear").clicked() {
                                    self.log.clear();
                                }
                                ui.weak(format!("{} entries", self.log.len()));
                            },
                        );
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for e in &self.log {
                                ui.label(format!(
                                    "{}  [{}]  {} ({}) = {}",
                                    e.time, e.provider, e.label, e.path, e.value
                                ));
                            }
                            if self.log.is_empty() {
                                ui.weak("Right-click a parameter → \"Log changes\" to record its value changes here.");
                            }
                        });
                });
        }

        self.meter_panel(ui);

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.tree_panel(ui);
        });
    }
}

impl App {
    fn sidebar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::SidePanel::left("addressbook")
            .resizable(true)
            .default_width(240.0)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Providers");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("➕").on_hover_text("Add provider").clicked() {
                            self.add = AddDialog {
                                open: true,
                                port: DEFAULT_PORT.to_string(),
                                parent: AddressBook::ROOT_ID,
                                ..Default::default()
                            };
                        }
                        if ui.button("📁+").on_hover_text("Add folder").clicked() {
                            self.folder_dialog = FolderDialog {
                                open: true,
                                parent: AddressBook::ROOT_ID,
                                ..Default::default()
                            };
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if !self.provider_filter.is_empty() && ui.small_button("✖").clicked() {
                        self.provider_filter.clear();
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.provider_filter)
                            .hint_text("🔍 filter")
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.separator();

                let root = self.book.root().clone();
                let filter = self.provider_filter.trim().to_lowercase();
                let mut action = None;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for child in &root.children {
                        Self::sidebar_node(
                            ui,
                            child,
                            &self.sessions,
                            self.active,
                            &mut action,
                            &filter,
                        );
                    }
                    ui.add_space(6.0);
                    // Drop zone to move a node out to the top level.
                    let (_, payload) = ui.dnd_drop_zone::<DragPayload, ()>(
                        egui::Frame::default().inner_margin(4.0),
                        |ui| {
                            ui.weak("▸ top level (drop here)");
                            ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
                        },
                    );
                    if let Some(p) = payload {
                        action = Some(SidebarAction::Move {
                            node: p.0,
                            into: AddressBook::ROOT_ID,
                        });
                    }
                });
                self.dispatch_sidebar(action, ctx);
            });
    }

    fn dispatch_sidebar(&mut self, action: Option<SidebarAction>, ctx: &egui::Context) {
        match action {
            Some(SidebarAction::Open(id)) => self.open_provider(id, ctx),
            Some(SidebarAction::Disconnect(id)) => self.disconnect(id),
            Some(SidebarAction::EditProvider(id)) => self.open_edit_dialog(id),
            Some(SidebarAction::Remove(id)) => self.remove_node(id),
            Some(SidebarAction::AddFolder(parent)) => {
                self.folder_dialog = FolderDialog {
                    open: true,
                    parent,
                    ..Default::default()
                };
            }
            Some(SidebarAction::RenameFolder(id)) => {
                let name = self
                    .book
                    .find(id)
                    .map(|n| n.name().to_string())
                    .unwrap_or_default();
                self.folder_dialog = FolderDialog {
                    open: true,
                    name,
                    rename: Some(id),
                    ..Default::default()
                };
            }
            Some(SidebarAction::Move { node, into }) => {
                let moved = self.book.move_node(node, into);
                if moved {
                    let _ = self.book.save();
                }
            }
            None => {}
        }
    }

    /// Render one address-book node (folder or provider) recursively.
    /// When `filter` is non-empty, only matching providers (and folders with a
    /// matching descendant) are shown, with those folders force-expanded.
    fn sidebar_node(
        ui: &mut egui::Ui,
        node: &Node,
        sessions: &HashMap<Id, Session>,
        active: Option<Id>,
        action: &mut Option<SidebarAction>,
        filter: &str,
    ) {
        match node {
            Node::Folder(folder) => {
                if !filter.is_empty() && !folder_matches(folder, filter) {
                    return;
                }
                let mut header =
                    egui::CollapsingHeader::new(format!("📁 {}", folder.name)).default_open(true);
                if !filter.is_empty() {
                    header = header.open(Some(true));
                }
                let resp = header.show(ui, |ui| {
                    for child in &folder.children {
                        Self::sidebar_node(ui, child, sessions, active, action, filter);
                    }
                });
                let hr = resp.header_response;
                // Drop onto a folder → move the dragged node into it.
                if let Some(p) = hr.dnd_release_payload::<DragPayload>() {
                    if p.0 != folder.id {
                        *action = Some(SidebarAction::Move {
                            node: p.0,
                            into: folder.id,
                        });
                    }
                }
                hr.context_menu(|ui| {
                    if ui.button("Add subfolder…").clicked() {
                        *action = Some(SidebarAction::AddFolder(folder.id));
                        ui.close();
                    }
                    if ui.button("Rename…").clicked() {
                        *action = Some(SidebarAction::RenameFolder(folder.id));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Remove folder").clicked() {
                        *action = Some(SidebarAction::Remove(folder.id));
                        ui.close();
                    }
                });
            }
            Node::Provider(p) => {
                if !filter.is_empty() && !p.name.to_lowercase().contains(filter) {
                    return;
                }
                let connected = sessions.contains_key(&p.id);
                let status = sessions.get(&p.id).map(|s| &s.status);
                let selected = active == Some(p.id);
                let label_resp = ui
                    .horizontal(|ui| {
                        paint_dot(ui, status_color(status));
                        // A label that senses click AND drag: click connects,
                        // drag moves it, secondary-click opens the menu.
                        let mut text = egui::RichText::new(&p.name);
                        if selected {
                            text = text.background_color(ui.visuals().selection.bg_fill);
                        }
                        ui.add(
                            egui::Label::new(text)
                                .selectable(false)
                                .sense(egui::Sense::click_and_drag()),
                        )
                        .on_hover_text(format!("{}:{}", p.host, p.port))
                    })
                    .inner;
                if label_resp.dragged() {
                    label_resp.dnd_set_drag_payload(DragPayload(p.id));
                }
                if label_resp.clicked() {
                    *action = Some(SidebarAction::Open(p.id));
                }
                label_resp.context_menu(|ui| {
                    if connected {
                        if ui.button("Disconnect").clicked() {
                            *action = Some(SidebarAction::Disconnect(p.id));
                            ui.close();
                        }
                    } else if ui.button("Connect").clicked() {
                        *action = Some(SidebarAction::Open(p.id));
                        ui.close();
                    }
                    if ui.button("Edit…").clicked() {
                        *action = Some(SidebarAction::EditProvider(p.id));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Remove").clicked() {
                        *action = Some(SidebarAction::Remove(p.id));
                        ui.close();
                    }
                });
            }
        }
    }

    /// Open the dialog pre-filled to edit an existing provider.
    fn open_edit_dialog(&mut self, id: Id) {
        if let Some(p) = self.book.find_provider(id) {
            self.add = AddDialog {
                open: true,
                name: p.name.clone(),
                host: p.host.clone(),
                port: p.port.to_string(),
                parent: AddressBook::ROOT_ID,
                editing: Some(id),
            };
        }
    }

    /// Remove a provider or folder; disconnect any providers it contained.
    fn remove_node(&mut self, id: Id) {
        if let Some(node) = self.book.remove(id) {
            let mut ids = Vec::new();
            collect_provider_ids(&node, &mut ids);
            for pid in ids {
                self.disconnect(pid);
            }
            if let Err(e) = self.book.save() {
                self.status_line = format!("could not save address book: {e}");
            }
        }
    }

    /// Create or rename a folder.
    fn folder_dialog(&mut self, ctx: &egui::Context) {
        if !self.folder_dialog.open {
            return;
        }
        let mut open = self.folder_dialog.open;
        let renaming = self.folder_dialog.rename;
        let title = if renaming.is_some() {
            "Rename folder"
        } else {
            "New folder"
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.folder_dialog.name)
                        .hint_text("folder name"),
                );
                resp.request_focus();
                let submit = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.separator();
                ui.horizontal(|ui| {
                    let valid = !self.folder_dialog.name.trim().is_empty();
                    if (ui.add_enabled(valid, egui::Button::new("OK")).clicked() || submit) && valid
                    {
                        let name = self.folder_dialog.name.trim().to_string();
                        match renaming {
                            Some(id) => {
                                self.book.rename(id, name);
                            }
                            None => {
                                self.book.add_folder(self.folder_dialog.parent, name);
                            }
                        }
                        let _ = self.book.save();
                        self.folder_dialog.open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.folder_dialog.open = false;
                    }
                });
            });
        if !open {
            self.folder_dialog.open = false;
        }
    }

    fn add_dialog(&mut self, ctx: &egui::Context) {
        if !self.add.open {
            return;
        }
        let mut open = self.add.open;
        let editing = self.add.editing;
        let title = if editing.is_some() {
            "Edit provider"
        } else {
            "Add provider"
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("addgrid").num_columns(2).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.add.name);
                    ui.end_row();
                    ui.label("Host");
                    ui.text_edit_singleline(&mut self.add.host);
                    ui.end_row();
                    ui.label("Port");
                    ui.text_edit_singleline(&mut self.add.port);
                    ui.end_row();
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let valid = !self.add.host.trim().is_empty();
                    let save_label = if editing.is_some() { "Save" } else { "Add" };
                    if ui
                        .add_enabled(valid, egui::Button::new(save_label))
                        .clicked()
                    {
                        let port = self.add.port.trim().parse().unwrap_or(DEFAULT_PORT);
                        let host = self.add.host.trim().to_string();
                        let name = if self.add.name.trim().is_empty() {
                            host.clone()
                        } else {
                            self.add.name.clone()
                        };
                        match editing {
                            Some(id) => {
                                self.book
                                    .update_provider(id, name.clone(), host, port, None);
                                // Reflect a new display name on an open tab.
                                if let Some(s) = self.sessions.get_mut(&id) {
                                    s.name = name;
                                }
                            }
                            None => {
                                self.book
                                    .add_provider(self.add.parent, name, host, port, None);
                            }
                        }
                        if let Err(e) = self.book.save() {
                            self.status_line = format!("could not save address book: {e}");
                        }
                        self.add.open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.add.open = false;
                    }
                });
            });
        // If the window was closed via its [x], reflect that.
        if !open {
            self.add.open = false;
        }
    }

    /// Send the deferred `false` of any boolean pulses whose hold time elapsed.
    fn process_pulses(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let mut soonest: Option<std::time::Duration> = None;
        for session in self.sessions.values_mut() {
            let due: Vec<Vec<u32>> = session
                .pulses
                .iter()
                .filter(|(_, t)| now >= **t)
                .map(|(p, _)| p.clone())
                .collect();
            for path in due {
                session.pulses.remove(&path);
                session
                    .handle
                    .send(NetCommand::SetValue(path, Value::Boolean(false)));
            }
            for t in session.pulses.values() {
                let d = t.saturating_duration_since(now);
                soonest = Some(soonest.map_or(d, |s| s.min(d)));
            }
        }
        if let Some(d) = soonest {
            ctx.request_repaint_after(d);
        }
    }

    fn options_window(&mut self, ctx: &egui::Context) {
        if !self.show_options {
            return;
        }
        let mut open = self.show_options;
        let mut changed = false;
        // Server start/stop is deferred until after the window closure (it needs
        // a clean &mut self).
        let mut do_sync = false;
        let mut do_restart = false;
        egui::Window::new("Options")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::Grid::new("optsgrid").num_columns(2).show(ui, |ui| {
                    ui.label("On startup");
                    egui::ComboBox::from_id_salt("startup")
                        .selected_text(self.settings.startup_mode.label())
                        .show_ui(ui, |ui| {
                            for m in [
                                StartupMode::ConnectNone,
                                StartupMode::ConnectAll,
                                StartupMode::ConnectLast,
                            ] {
                                changed |= ui
                                    .selectable_value(&mut self.settings.startup_mode, m, m.label())
                                    .clicked();
                            }
                        });
                    ui.end_row();

                    ui.label("Order by");
                    egui::ComboBox::from_id_salt("orderby")
                        .selected_text(self.settings.order_by.label())
                        .show_ui(ui, |ui| {
                            for o in [OrderBy::Number, OrderBy::Identifier, OrderBy::Description] {
                                changed |= ui
                                    .selectable_value(&mut self.settings.order_by, o, o.label())
                                    .clicked();
                            }
                        });
                    ui.end_row();

                    ui.label("Boolean pulse (ms)");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut self.settings.boolean_pulse_ms)
                                .range(10..=10_000),
                        )
                        .changed();
                    ui.end_row();

                    ui.label("Log file");
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.settings.log_file)
                                .hint_text("optional path"),
                        )
                        .changed();
                    ui.end_row();
                });
                ui.separator();
                changed |= ui
                    .checkbox(
                        &mut self.settings.show_descriptions,
                        "Show descriptions in tree",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.settings.clear_tree_on_disconnect,
                        "Clear tree on disconnect",
                    )
                    .changed();
                changed |= ui
                    .checkbox(&mut self.settings.send_keepalive, "Send keep-alive")
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.settings.matrix_targets_on_top,
                        "Matrix: targets on top (else sources on top)",
                    )
                    .changed();
                if ui
                    .checkbox(&mut self.settings.dark_mode, "Dark mode")
                    .changed()
                {
                    apply_theme(ctx, self.settings.dark_mode);
                    self.applied_dark = Some(self.settings.dark_mode);
                    changed = true;
                }

                ui.separator();
                ui.label(egui::RichText::new("Server mode (browser access)").strong());
                if ui
                    .checkbox(&mut self.settings.server_enabled, "Enable web server")
                    .on_hover_text("Serve this instance to browsers on your network")
                    .changed()
                {
                    changed = true;
                    do_sync = true;
                }
                ui.horizontal(|ui| {
                    ui.label("Port");
                    if ui
                        .add(egui::DragValue::new(&mut self.settings.server_port).range(1..=65535))
                        .changed()
                    {
                        changed = true;
                        do_restart = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Token");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.settings.server_token)
                                .desired_width(160.0),
                        )
                        .changed()
                    {
                        changed = true;
                        do_restart = true;
                    }
                    if ui.button("Regenerate").clicked() {
                        self.settings.server_token = generate_token();
                        changed = true;
                        do_restart = true;
                    }
                });
                if ui
                    .checkbox(
                        &mut self.settings.server_open_lan,
                        "Open on LAN (no token — anyone can control)",
                    )
                    .changed()
                {
                    changed = true;
                    do_restart = true;
                }
                if ui
                    .checkbox(&mut self.settings.server_read_only, "Web clients read-only")
                    .changed()
                {
                    changed = true;
                    do_restart = true;
                }
                if let Some(srv) = &self.server {
                    ui.colored_label(ACCENT, format!("● listening on port {}", srv.bound.port()));
                    if !self.settings.server_open_lan && ui.button("Copy access token").clicked() {
                        ui.ctx().copy_text(self.settings.server_token.clone());
                    }
                }
            });
        self.show_options = open;
        if do_sync {
            self.sync_server();
        } else if do_restart {
            self.restart_server();
        }
        if changed {
            if let Err(e) = self.settings.save() {
                self.status_line = format!("could not save settings: {e}");
            }
        }
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut open = self.show_about;
        egui::Window::new("About")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.heading(egui::RichText::new("emberviewer").color(ACCENT));
                ui.label("A cross-platform Ember+ viewer");
                ui.add_space(4.0);
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("An open replacement for Lawo's EmberPlusView.");
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                ui.hyperlink_to(
                    "GitHub repository",
                    "https://github.com/mattlamb99/emberviewer",
                );
                ui.hyperlink_to("Website / docs", "https://mattlamb99.github.io/emberviewer");
            });
        self.show_about = open;
    }

    /// Popup showing a matrix signal's parameters (gain, type, name, …), opened
    /// by clicking a row/column header in the matrix grid.
    fn signal_params_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.active else {
            return;
        };
        let opts = RenderOpts {
            pulse_ms: self.settings.boolean_pulse_ms,
            show_descriptions: self.settings.show_descriptions,
            order_by: self.settings.order_by,
            matrix_targets_on_top: self.settings.matrix_targets_on_top,
        };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let Some((node_path, title)) = session.signal_params.clone() else {
            return;
        };
        let mut open = true;
        let mut commands: Vec<NetCommand> = Vec::new();
        egui::Window::new(format!("Signal · {title}"))
            .open(&mut open)
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                let Some(node) = session.tree.get(&node_path).cloned() else {
                    // Defensive re-fetch (the click handler usually issues this).
                    if session.label_fetch.insert(node_path.clone()) {
                        commands.push(NetCommand::GetDirectory(node_path.clone()));
                    }
                    ui.weak("loading…");
                    return;
                };
                let children = sorted_paths(&session.tree, &node.children, opts.order_by);
                if children.is_empty() {
                    ui.weak("loading…");
                    return;
                }
                egui::Grid::new(("sigparams", &node_path))
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for cp in &children {
                            let Some(ce) = session.tree.get(cp).cloned() else {
                                continue;
                            };
                            ui.label(egui::RichText::new(&ce.identifier).strong());
                            editor(ui, session, &ce, &opts, &mut commands);
                            ui.end_row();
                        }
                    });
            });
        if !open {
            session.signal_params = None;
        }
        for cmd in commands {
            session.handle.send(cmd);
        }
    }

    fn toggle_discovery(&mut self) {
        if self.discovery.is_some() {
            self.discovery = None;
            self.show_discovery = false;
        } else {
            match crate::discovery::Discovery::start() {
                Ok(d) => {
                    self.discovery = Some(d);
                    self.show_discovery = true;
                }
                Err(e) => self.status_line = format!("discovery failed: {e}"),
            }
        }
    }

    /// Drain mDNS events and keep the UI repainting while browsing.
    fn poll_discovery(&mut self, ctx: &egui::Context) {
        if let Some(d) = &mut self.discovery {
            d.poll();
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn discovery_window(&mut self, ctx: &egui::Context) {
        if !self.show_discovery {
            return;
        }
        let mut open = self.show_discovery;
        let mut to_add: Option<crate::discovery::Discovered> = None;
        egui::Window::new("Discover providers (mDNS)")
            .open(&mut open)
            .default_width(360.0)
            .show(ctx, |ui| {
                let found = self
                    .discovery
                    .as_ref()
                    .map(|d| d.sorted())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Browsing _ember._tcp … {} found", found.len()));
                });
                ui.separator();
                if found.is_empty() {
                    ui.weak("No providers found yet. Ensure they're on this network.");
                }
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for d in &found {
                            ui.horizontal(|ui| {
                                let exists = self.book_has_host(&d.host, d.port);
                                if ui
                                    .add_enabled(!exists, egui::Button::new("Add"))
                                    .on_hover_text(if exists {
                                        "already in address book"
                                    } else {
                                        ""
                                    })
                                    .clicked()
                                {
                                    to_add = Some(d.clone());
                                }
                                ui.label(format!("{}  ", d.display_name()));
                                ui.weak(format!("{}:{}", d.host, d.port));
                            });
                        }
                    });
            });
        self.show_discovery = open;
        if !open {
            self.discovery = None;
        }
        if let Some(d) = to_add {
            self.book
                .add_provider(AddressBook::ROOT_ID, d.display_name(), d.host, d.port, None);
            let _ = self.book.save();
        }
    }

    /// Whether a provider with this host:port already exists in the book.
    fn book_has_host(&self, host: &str, port: u16) -> bool {
        self.book
            .iter()
            .any(|(_, n)| matches!(n, Node::Provider(p) if p.host == host && p.port == port))
    }

    /// Right-side vertical meter for the active session's selected parameter.
    fn meter_panel(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.active else { return };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let Some(path) = session.selected.clone() else {
            return;
        };
        let Some(entry) = session.tree.get(&path).cloned() else {
            return;
        };
        if !is_meterable(&entry) {
            return;
        }
        let range = meter_range(&entry, &mut session.meter_range);
        let value = entry.value.as_ref().and_then(value_f64);
        egui::SidePanel::right("meterpanel")
            .exact_width(96.0)
            .show_inside(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(entry.label()).small().strong());
                    let h = (ui.available_height() - 22.0).max(40.0);
                    draw_vmeter(ui, value, range, 30.0, h);
                    if let Some(v) = value {
                        ui.label(egui::RichText::new(format!("{v:.2}")).small());
                    }
                });
            });
    }

    /// Render each popped-out meter as its own (optionally always-on-top) window.
    fn popped_meters(&mut self, ctx: &egui::Context) {
        let Some(id) = self.active else { return };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let items: Vec<(usize, Vec<u32>, bool)> = session
            .popped
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.path.clone(), p.always_on_top))
            .collect();
        let mut to_close = Vec::new();
        let mut to_toggle = Vec::new();
        for (i, path, aot) in items {
            let Some(entry) = session.tree.get(&path).cloned() else {
                continue;
            };
            let range = meter_range(&entry, &mut session.meter_range);
            let value = entry.value.as_ref().and_then(value_f64);
            let title = entry.label();
            let vp_id = egui::ViewportId::from_hash_of(("popmeter", id, &path));
            let mut builder = egui::ViewportBuilder::default()
                .with_title(format!("{title} — meter"))
                .with_inner_size([130.0, 280.0])
                .with_min_inner_size([90.0, 140.0]);
            if aot {
                builder = builder.with_always_on_top();
            }
            let mut close = false;
            let mut toggle = false;
            ctx.show_viewport_immediate(vp_id, builder, |ctx, _| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(&title).strong());
                        let h = (ui.available_height() - 46.0).max(50.0);
                        let resp = draw_vmeter(ui, value, range, 40.0, h);
                        if let Some(v) = value {
                            ui.label(format!("{v:.3}"));
                        }
                        resp.context_menu(|ui| {
                            let l = if aot {
                                "Unpin (always on top)"
                            } else {
                                "Always on top"
                            };
                            if ui.button(l).clicked() {
                                toggle = true;
                                ui.close();
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                                ui.close();
                            }
                        });
                    });
                });
                if ctx.input(|i| i.viewport().close_requested()) {
                    close = true;
                }
            });
            if close {
                to_close.push(i);
            }
            if toggle {
                to_toggle.push(i);
            }
        }
        for i in to_toggle {
            if let Some(p) = session.popped.get_mut(i) {
                p.always_on_top = !p.always_on_top;
            }
        }
        for i in to_close.into_iter().rev() {
            if i < session.popped.len() {
                session.popped.remove(i);
            }
        }
    }

    /// One tab per open connection, with status dot and a close button.
    fn tabs(&mut self, ui: &mut egui::Ui) {
        if self.sessions.is_empty() {
            return;
        }
        // Stable, deterministic tab order.
        let mut ids: Vec<Id> = self.sessions.keys().copied().collect();
        ids.sort_unstable();

        let mut activate = None;
        let mut disconnect = None;
        egui::TopBottomPanel::top("tabs").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                for id in &ids {
                    let session = &self.sessions[id];
                    let selected = self.active == Some(*id);
                    paint_dot(ui, status_color(Some(&session.status)));
                    if ui.selectable_label(selected, &session.name).clicked() {
                        activate = Some(*id);
                    }
                    if ui.small_button("✖").on_hover_text("Disconnect").clicked() {
                        disconnect = Some(*id);
                    }
                    ui.separator();
                }
            });
        });
        if let Some(id) = activate {
            self.active = Some(id);
        }
        if let Some(id) = disconnect {
            self.disconnect(id);
        }
    }

    fn tree_panel(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.active else {
            ui.centered_and_justified(|ui| {
                ui.label("Select a provider to connect.");
            });
            return;
        };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };

        ui.horizontal(|ui| {
            ui.heading(&session.name);
            ui.label(
                egui::RichText::new(format!("({})", session.addr))
                    .weak()
                    .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !session.filter.is_empty() && ui.button("✖").clicked() {
                    session.filter.clear();
                }
                ui.add(
                    egui::TextEdit::singleline(&mut session.filter)
                        .hint_text("🔍 filter")
                        .desired_width(160.0),
                );
            });
        });
        ui.separator();

        // Build the allowed-path set when filtering (loaded entries only).
        let filter_set = if session.filter.trim().is_empty() {
            None
        } else {
            Some(compute_filter_set(&session.tree, session.filter.trim()))
        };

        let opts = RenderOpts {
            pulse_ms: self.settings.boolean_pulse_ms,
            show_descriptions: self.settings.show_descriptions,
            order_by: self.settings.order_by,
            matrix_targets_on_top: self.settings.matrix_targets_on_top,
        };

        // Collect commands to send, and the set of parameters currently on
        // screen (so we can manage subscriptions), after the render borrow ends.
        let mut commands: Vec<NetCommand> = Vec::new();
        let mut visible: Vec<Vec<u32>> = Vec::new();
        let roots = sorted_paths(&session.tree, &session.tree.roots, opts.order_by);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(allowed) = &filter_set {
                    if allowed.is_empty() {
                        ui.weak("no matches in loaded tree");
                    }
                    let mut row = 0usize;
                    for root in &roots {
                        render_filtered(
                            ui,
                            session,
                            root,
                            allowed,
                            &opts,
                            true,
                            &mut row,
                            &mut commands,
                            &mut visible,
                        );
                    }
                } else {
                    let mut row = 0usize;
                    for root in &roots {
                        render_entry(
                            ui,
                            session,
                            root,
                            &opts,
                            true,
                            &mut row,
                            &mut commands,
                            &mut visible,
                        );
                    }
                }
            });

        // Subscribe to newly-visible parameters; unsubscribe ones now hidden.
        let visible: HashSet<Vec<u32>> = visible.into_iter().collect();
        for path in &visible {
            if !session.subscribed.contains(path) {
                commands.push(NetCommand::Subscribe(path.clone()));
            }
        }
        for path in &session.subscribed {
            if !visible.contains(path) {
                commands.push(NetCommand::Unsubscribe(path.clone()));
            }
        }
        session.subscribed = visible;

        for cmd in commands {
            session.handle.send(cmd);
        }
    }
}

/// Recursively render a tree entry; pushes network commands to `commands` and
/// records on-screen parameter paths in `visible`.
#[allow(clippy::too_many_arguments)]
fn render_entry(
    ui: &mut egui::Ui,
    session: &mut Session,
    path: &[u32],
    opts: &RenderOpts,
    online: bool,
    row: &mut usize,
    commands: &mut Vec<NetCommand>,
    visible: &mut Vec<Vec<u32>>,
) {
    let Some(entry) = session.tree.get(path).cloned() else {
        return;
    };
    let eff_online = online && entry.is_online;

    if entry.kind.is_expandable() {
        let is_open = session.open.get(path).copied().unwrap_or(false);
        let id = ui.make_persistent_id(("node", path));
        let header =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        let next_open = header.is_open();
        let mut heading = egui::RichText::new(node_label(&entry, opts));
        if !eff_online {
            heading = heading.weak().italics();
        }
        let resp = header
            .show_header(ui, |ui| {
                ui.label(heading)
                    .on_hover_text(if eff_online { "" } else { "offline" })
                    .context_menu(|ui| context_copy(ui, &entry.path, &entry.identifier));
            })
            .body(|ui| {
                // Matrix grid / function form, when this node is one.
                if let Some(m) = &entry.matrix {
                    // The matrix's connections/targets/sources arrive from a
                    // getDirectory on the matrix itself (not on its parent) — fetch
                    // it once when the grid first shows.
                    if session.label_fetch.insert(entry.path.clone()) {
                        commands.push(NetCommand::GetMatrixDirectory(entry.path.clone()));
                    }
                    // Eagerly fetch each label sub-tree: basePath → targets/sources
                    // → string params (number = signal id, value = name).
                    let bases = m.label_paths.clone();
                    for base in &bases {
                        fetch_label_subtree(session, base, commands);
                    }
                    // Eagerly fetch the parameters-location node (gain/name/type
                    // params) so the Matrix Params subtree populates on open.
                    if let Some(ploc) = m.params_location.clone() {
                        fetch_label_subtree(session, &ploc, commands);
                    }
                    if let Some((is_target, sig)) = crate::matrix_view::render_matrix(
                        ui,
                        &entry,
                        m,
                        opts.matrix_targets_on_top,
                        commands,
                    ) {
                        // Open the clicked signal's parameter node (gain/type/…).
                        let base = if is_target {
                            m.param_targets_path.clone()
                        } else {
                            m.param_sources_path.clone()
                        };
                        if let Some(mut node) = base {
                            node.push(sig);
                            if session.label_fetch.insert(node.clone()) {
                                commands.push(NetCommand::GetDirectory(node.clone()));
                            }
                            let kind = if is_target { "target" } else { "source" };
                            session.signal_params = Some((node, format!("{kind} {sig}")));
                        }
                    }
                } else if let Some(f) = &entry.function {
                    render_function(
                        ui,
                        &entry,
                        f,
                        &mut session.func_inputs,
                        &mut session.invocations,
                        &mut session.next_invocation_id,
                        &session.tree.invocation_results,
                        commands,
                    );
                }
                let children = sorted_paths(&session.tree, &entry.children, opts.order_by);
                for child in &children {
                    render_entry(ui, session, child, opts, eff_online, row, commands, visible);
                }
                if children.is_empty() && entry.matrix.is_none() && entry.function.is_none() {
                    ui.weak("…");
                }
            });
        // Lazily request children whenever a node is open but not yet fetched.
        // (`requested` is reset on reconnect, so this also drives re-discovery.)
        session.open.insert(path.to_vec(), next_open);
        let _ = (resp, is_open);
        if next_open && !entry.requested {
            if let Some(e) = session.tree.entries.get_mut(path) {
                e.requested = true;
            }
            if entry.matrix.is_some() {
                commands.push(NetCommand::GetMatrixDirectory(path.to_vec()));
            } else {
                commands.push(NetCommand::GetDirectory(path.to_vec()));
            }
        }
    } else {
        // A leaf parameter is on screen → eligible for a live subscription.
        visible.push(path.to_vec());
        render_parameter(ui, session, &entry, opts, eff_online, row, commands);
    }
}

/// Render a parameter row: label, value, and (if writable) an editor.
fn render_parameter(
    ui: &mut egui::Ui,
    session: &mut Session,
    entry: &crate::model::Entry,
    opts: &RenderOpts,
    online: bool,
    row: &mut usize,
    commands: &mut Vec<NetCommand>,
) {
    let label = param_label(entry, opts);
    let logging = session.logged.contains(&entry.path);
    let eff_online = online && entry.is_online;
    let selected = session.selected.as_deref() == Some(entry.path.as_slice());
    let meterable = is_meterable(entry);

    // Reserve a background shape so striping/selection paints behind the row.
    let bg = ui.painter().add(egui::Shape::Noop);
    let resp = ui
        .horizontal(|ui| {
            if !eff_online {
                ui.disable();
            }
            ui.add_space(8.0);
            if logging {
                paint_dot(ui, egui::Color32::from_rgb(210, 60, 60));
            }
            let (badge, color) = if entry.is_writable() {
                ("rw", egui::Color32::from_rgb(70, 130, 200))
            } else {
                ("ro", egui::Color32::from_gray(140))
            };
            ui.label(egui::RichText::new(badge).monospace().small().color(color))
                .on_hover_text(if entry.is_writable() {
                    "writable"
                } else {
                    "read-only"
                });

            // Clicking the name selects the parameter (drives the meter panel).
            let name = ui
                .selectable_label(selected, egui::RichText::new(label).strong())
                .on_hover_text(format!("path {}", path_string(&entry.path)));
            if name.clicked() {
                session.selected = Some(entry.path.clone());
            }
            name.context_menu(|ui| {
                context_copy(ui, &entry.path, &entry.identifier);
                if let Some(v) = &entry.value {
                    if ui.button("Copy value").clicked() {
                        ui.ctx().copy_text(crate::model::format_value(v));
                        ui.close();
                    }
                }
                ui.separator();
                let l = if logging {
                    "Stop logging"
                } else {
                    "Log changes"
                };
                if ui.button(l).clicked() {
                    if logging {
                        session.logged.remove(&entry.path);
                    } else {
                        session.logged.insert(entry.path.clone());
                    }
                    ui.close();
                }
                if meterable && ui.button("Pop out meter").clicked() {
                    if !session.popped.iter().any(|p| p.path == entry.path) {
                        session.popped.push(PoppedMeter {
                            path: entry.path.clone(),
                            always_on_top: false,
                        });
                    }
                    ui.close();
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Keep values clear of the (Windows) scrollbar.
                ui.add_space(14.0);
                if entry.param_type == Some(glow::parameter_type::TRIGGER) {
                    if ui.button("Fire").clicked() {
                        commands.push(NetCommand::SetValue(entry.path.clone(), Value::Integer(0)));
                    }
                } else if entry.is_writable() {
                    editor(ui, session, entry, opts, commands);
                } else if let Some(v) = &entry.value {
                    ui.label(display_value(entry, v));
                } else {
                    ui.weak("—");
                }
            });
        })
        .response;

    // Paint selection highlight or a faint stripe behind the row.
    let row_rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), resp.rect.y_range());
    if selected {
        ui.painter().set(
            bg,
            egui::Shape::rect_filled(
                row_rect,
                0.0,
                ui.visuals().selection.bg_fill.gamma_multiply(0.35),
            ),
        );
    } else if *row % 2 == 1 {
        let stripe = if ui.visuals().dark_mode {
            egui::Color32::from_white_alpha(8)
        } else {
            egui::Color32::from_black_alpha(8)
        };
        ui.painter()
            .set(bg, egui::Shape::rect_filled(row_rect, 0.0, stripe));
    }
    *row += 1;
}

/// A type-appropriate inline editor for a writable parameter.
fn editor(
    ui: &mut egui::Ui,
    session: &mut Session,
    entry: &crate::model::Entry,
    opts: &RenderOpts,
    commands: &mut Vec<NetCommand>,
) {
    let path = entry.path.clone();
    match &entry.value {
        Some(Value::Boolean(b)) => {
            // Set true / Set false / Pulse (true now, false after the hold time).
            let on = *b;
            ui.label(if on { "true" } else { "false" });
            if ui
                .button("Pulse")
                .on_hover_text("Set true, then false")
                .clicked()
            {
                commands.push(NetCommand::SetValue(path.clone(), Value::Boolean(true)));
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(opts.pulse_ms);
                session.pulses.insert(path.clone(), deadline);
            }
            if ui.add_enabled(on, egui::Button::new("Set false")).clicked() {
                session.pulses.remove(&path);
                commands.push(NetCommand::SetValue(path.clone(), Value::Boolean(false)));
            }
            if ui.add_enabled(!on, egui::Button::new("Set true")).clicked() {
                commands.push(NetCommand::SetValue(path, Value::Boolean(true)));
            }
        }
        Some(Value::Integer(i)) => {
            let i = *i;
            // Enum → combo of (non-hidden) labels; ranged → slider; else a field.
            if !entry.enum_entries.is_empty() {
                let current = entry
                    .enum_label(i)
                    .map(str::to_string)
                    .unwrap_or_else(|| i.to_string());
                egui::ComboBox::from_id_salt(("enum", &entry.path))
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for ent in entry.enum_entries.iter().filter(|e| !e.hidden) {
                            let mut sel = i;
                            if ui
                                .selectable_value(&mut sel, ent.value, &ent.label)
                                .clicked()
                            {
                                commands.push(NetCommand::SetValue(
                                    path.clone(),
                                    Value::Integer(ent.value),
                                ));
                            }
                        }
                    });
            } else if let (Some(Value::Integer(lo)), Some(Value::Integer(hi))) =
                (&entry.minimum, &entry.maximum)
            {
                let mut v = i;
                let factor = entry.factor.unwrap_or(1).max(1) as f64;
                let suffix = format_suffix(entry);
                let resp = ui.add(
                    egui::Slider::new(&mut v, *lo..=*hi)
                        .custom_formatter(move |n, _| format!("{}{}", n / factor, suffix)),
                );
                if resp.changed() {
                    commands.push(NetCommand::SetValue(path, Value::Integer(v)));
                }
            } else {
                text_commit_editor(ui, session, entry, commands, |s| {
                    s.trim().parse::<i64>().ok().map(Value::Integer)
                });
            }
        }
        Some(Value::Real(r)) => {
            if let (Some(Value::Real(lo)), Some(Value::Real(hi))) = (&entry.minimum, &entry.maximum)
            {
                let mut v = r.to_f64();
                let suffix = format_suffix(entry);
                let resp = ui.add(
                    egui::Slider::new(&mut v, lo.to_f64()..=hi.to_f64())
                        .custom_formatter(move |n, _| format!("{n:.3}{suffix}")),
                );
                if resp.changed() {
                    commands.push(NetCommand::SetValue(path, Value::Real(v.into())));
                }
            } else {
                text_commit_editor(ui, session, entry, commands, |s| {
                    s.trim().parse::<f64>().ok().map(|f| Value::Real(f.into()))
                });
            }
        }
        Some(Value::String(_)) | None => {
            text_commit_editor(ui, session, entry, commands, |s| {
                Some(Value::String(s.to_string()))
            });
        }
        Some(Value::Octets(_)) => {
            ui.weak("<octets>");
        }
    }
}

/// The literal text after a printf conversion specifier in a parameter's format
/// (e.g. "%d dB" → " dB"), used as a slider/value unit suffix.
fn format_suffix(entry: &crate::model::Entry) -> String {
    let Some(fmt) = &entry.format else {
        return String::new();
    };
    if let Some(pct) = fmt.find('%') {
        let rest = &fmt[pct + 1..];
        if let Some(conv) = rest.find(|c: char| "diouxXeEfFgGsc".contains(c)) {
            return rest[conv + 1..].to_string();
        }
    }
    String::new()
}

/// Human-readable value: enum label, or factor/format-applied number, else raw.
fn display_value(entry: &crate::model::Entry, v: &Value) -> String {
    match v {
        Value::Integer(i) => {
            if let Some(lbl) = entry.enum_label(*i) {
                lbl.to_string()
            } else {
                let factor = entry.factor.unwrap_or(1).max(1);
                if factor != 1 {
                    format!("{}{}", *i as f64 / factor as f64, format_suffix(entry))
                } else if entry.format.is_some() {
                    format!("{i}{}", format_suffix(entry))
                } else {
                    i.to_string()
                }
            }
        }
        Value::Real(r) if entry.format.is_some() => {
            format!("{}{}", r.to_f64(), format_suffix(entry))
        }
        _ => format_value(v),
    }
}

/// Eagerly fetch a matrix label sub-tree so source/target names resolve:
/// request the base node, then its `targets`/`sources` children (whose
/// getDirectory returns the label string params). Deduped via `label_fetch`.
fn fetch_label_subtree(session: &mut Session, base: &[u32], commands: &mut Vec<NetCommand>) {
    // Always getDirectory the base once — it usually exists as a stub (created by
    // the matrix's parent fetch) with no children loaded yet, so checking
    // is_none() would never fetch its targets/sources.
    if session.label_fetch.insert(base.to_vec()) {
        commands.push(NetCommand::GetDirectory(base.to_vec()));
    }
    // Once the base's children (targets/sources) arrive, fetch them too (their
    // getDirectory returns the label string params).
    let children = session
        .tree
        .get(base)
        .map(|e| e.children.clone())
        .unwrap_or_default();
    for child in children {
        if session.label_fetch.insert(child.clone()) {
            commands.push(NetCommand::GetDirectory(child));
        }
    }
}

/// Paint a small filled status dot inline (drawn, not a font glyph, so it always
/// renders regardless of the available fonts).
fn paint_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(12.0, ui.spacing().interact_size.y),
        egui::Sense::hover(),
    );
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

/// Collect every provider id within a node subtree.
fn collect_provider_ids(node: &Node, out: &mut Vec<Id>) {
    match node {
        Node::Provider(p) => out.push(p.id),
        Node::Folder(f) => {
            for child in &f.children {
                collect_provider_ids(child, out);
            }
        }
    }
}

/// Whether a folder matches the sidebar filter (by its own name or any descendant).
fn folder_matches(folder: &crate::address_book::Folder, filter: &str) -> bool {
    folder.name.to_lowercase().contains(filter)
        || folder.children.iter().any(|c| node_matches(c, filter))
}

fn node_matches(node: &Node, filter: &str) -> bool {
    match node {
        Node::Folder(f) => folder_matches(f, filter),
        Node::Provider(p) => p.name.to_lowercase().contains(filter),
    }
}

/// A status-indicator colour chosen to read on both light and dark themes.
fn status_color(status: Option<&Status>) -> egui::Color32 {
    use egui::Color32;
    match status {
        Some(Status::Connected) => Color32::from_rgb(40, 160, 80),
        Some(Status::Connecting) => Color32::from_rgb(190, 140, 0),
        Some(Status::Reconnecting { .. }) => Color32::from_rgb(210, 120, 20),
        Some(Status::Disconnected(_)) => Color32::from_rgb(200, 60, 60),
        None => Color32::GRAY,
    }
}

/// Dotted path string, e.g. `0.1.2`.
fn path_string(path: &[u32]) -> String {
    path.iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Right-click menu: copy the element's path or identifier to the clipboard.
fn context_copy(ui: &mut egui::Ui, path: &[u32], identifier: &str) {
    if ui.button("Copy path").clicked() {
        ui.ctx().copy_text(path_string(path));
        ui.close();
    }
    if !identifier.is_empty() && ui.button("Copy identifier").clicked() {
        ui.ctx().copy_text(identifier.to_string());
        ui.close();
    }
}

/// Set of paths to show when filtering: every entry whose identifier matches the
/// (case-insensitive) query, plus all of their ancestors and descendants, so
/// matches keep their context and matched nodes reveal their subtree.
fn compute_filter_set(tree: &TreeModel, query: &str) -> HashSet<Vec<u32>> {
    let q = query.to_lowercase();
    let matches: Vec<&Vec<u32>> = tree
        .entries
        .values()
        .filter(|e| e.label().to_lowercase().contains(&q))
        .map(|e| &e.path)
        .collect();

    let mut allowed = HashSet::new();
    for m in &matches {
        // Ancestors (every prefix) + the match itself.
        for len in 1..=m.len() {
            allowed.insert(m[..len].to_vec());
        }
    }
    // Descendants of any match.
    for entry in tree.entries.keys() {
        if matches
            .iter()
            .any(|m| entry.len() > m.len() && entry.starts_with(m))
        {
            allowed.insert(entry.clone());
        }
    }
    allowed
}

/// Render a filtered tree: only `allowed` paths, always expanded. Records
/// visible parameter paths so subscriptions still track what's on screen.
#[allow(clippy::too_many_arguments)]
fn render_filtered(
    ui: &mut egui::Ui,
    session: &mut Session,
    path: &[u32],
    allowed: &HashSet<Vec<u32>>,
    opts: &RenderOpts,
    online: bool,
    row: &mut usize,
    commands: &mut Vec<NetCommand>,
    visible: &mut Vec<Vec<u32>>,
) {
    if !allowed.contains(path) {
        return;
    }
    let Some(entry) = session.tree.get(path).cloned() else {
        return;
    };
    let eff_online = online && entry.is_online;
    if entry.kind.is_expandable() {
        ui.label(node_label(&entry, opts))
            .context_menu(|ui| context_copy(ui, &entry.path, &entry.identifier));
        ui.indent(("filt", path), |ui| {
            let children = sorted_paths(&session.tree, &entry.children, opts.order_by);
            for child in &children {
                render_filtered(
                    ui, session, child, allowed, opts, eff_online, row, commands, visible,
                );
            }
        });
    } else {
        visible.push(path.to_vec());
        render_parameter(ui, session, &entry, opts, eff_online, row, commands);
    }
}

/// Heading text for an expandable node (with optional description suffix).
fn node_label(entry: &crate::model::Entry, opts: &RenderOpts) -> String {
    let base = format!("📁 {}", entry.label());
    append_description(base, entry, opts)
}

/// Label text for a parameter (with optional description suffix).
fn param_label(entry: &crate::model::Entry, opts: &RenderOpts) -> String {
    append_description(entry.label(), entry, opts)
}

fn append_description(base: String, entry: &crate::model::Entry, opts: &RenderOpts) -> String {
    match (opts.show_descriptions, &entry.description) {
        (true, Some(d)) if !d.is_empty() => format!("{base}  —  {d}"),
        _ => base,
    }
}

/// Order a set of sibling paths according to `order`. Unknown entries keep their
/// given order at the end.
fn sorted_paths(tree: &TreeModel, paths: &[Vec<u32>], order: OrderBy) -> Vec<Vec<u32>> {
    let mut out = paths.to_vec();
    out.sort_by(|a, b| {
        let (ea, eb) = (tree.get(a), tree.get(b));
        match order {
            OrderBy::Number => a.last().cmp(&b.last()),
            OrderBy::Identifier => key_of(ea, |e| e.identifier.to_lowercase())
                .cmp(&key_of(eb, |e| e.identifier.to_lowercase())),
            OrderBy::Description => key_of(ea, |e| {
                e.description.clone().unwrap_or_default().to_lowercase()
            })
            .cmp(&key_of(eb, |e| {
                e.description.clone().unwrap_or_default().to_lowercase()
            })),
        }
    });
    out
}

fn key_of(
    entry: Option<&crate::model::Entry>,
    f: impl Fn(&crate::model::Entry) -> String,
) -> String {
    entry.map(f).unwrap_or_default()
}

/// A text field that commits its parsed value on Enter / focus-loss.
fn text_commit_editor(
    ui: &mut egui::Ui,
    session: &mut Session,
    entry: &crate::model::Entry,
    commands: &mut Vec<NetCommand>,
    parse: impl Fn(&str) -> Option<Value>,
) {
    let path = entry.path.clone();
    let buf = session
        .edits
        .entry(path.clone())
        .or_insert_with(|| entry.value.as_ref().map(format_value).unwrap_or_default());
    let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(140.0));
    let commit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if commit {
        if let Some(v) = parse(buf) {
            commands.push(NetCommand::SetValue(path.clone(), v));
        }
        // Clear so the field re-syncs to the echoed value next frame.
        session.edits.remove(&path);
    } else if !resp.has_focus() {
        // Keep the field in sync with the live value while not being edited.
        if let Some(v) = &entry.value {
            let live = format_value(v);
            if buf != &live {
                *buf = live;
            }
        }
    }
}
