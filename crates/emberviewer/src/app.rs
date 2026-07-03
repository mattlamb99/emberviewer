//! The eframe application: address-book sidebar + provider tree browser.
//!
//! egui 0.34 deprecated the `SidePanel`/`TopBottomPanel` aliases in favour of a
//! unified `Panel` API; the aliases still work, so we keep them for now and
//! migrate when that API settles.
#![allow(deprecated)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ember_proto::glow::{self, Value};
use ember_web_proto::{WireNode, WireProvider};

use crate::address_book::{AddressBook, Id, Node, DEFAULT_PORT};
use crate::hub::HubRegistry;
use crate::model::{format_value, label_fetch_step, TreeModel, LABEL_FETCH_RETRY_SECS};
use crate::net::{ConnectionHandle, NetCommand, NetEvent};
use crate::server::{self, ServerHandle};
use crate::settings::{OrderBy, Settings, StartupMode};
use crate::widgets::{
    clean_multiline, display_value, draw_indicator, draw_vmeter, format_suffix, is_meterable,
    lock_toggle, lockable, meter_range, meter_readout, render_function, string_edit_window,
    value_f64, StringEdit, LOCK_FLASH_SECS,
};

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
    /// Per label sub-tree node: (egui time of last request, attempts so far).
    /// Embedded devices silently drop getDirectory requests under the initial
    /// discovery burst, so a one-shot fetch can be lost forever; we re-request
    /// (throttled, capped) until the node actually reports children.
    label_retry: HashMap<Vec<u32>, (f64, u8)>,
    /// Open "signal parameters" popup: the signal's parameter node path + a title.
    signal_params: Option<(Vec<u32>, String)>,
    /// Open multi-line string editor/viewer, if any.
    string_edit: Option<StringEdit>,
    /// Paths set optimistically on the UI side and not yet confirmed by a value
    /// update from the provider. Rendered as "pending" so an unconfirmed value is
    /// distinguishable from an authoritative one.
    pending: HashSet<Vec<u32>>,
    /// egui time until which the padlock flashes (set when a locked control is
    /// clicked, to explain why the action did nothing).
    flash_until: f64,
    /// Last sampled socket totals and the time of that sample (for rate calc).
    traffic_prev: ember_net::TrafficSnapshot,
    traffic_t: f64,
    /// Smoothed TX/RX rates toward the device, recomputed ~once a second.
    rate: TrafficRate,
}

/// TX/RX rates toward a device: bytes and S101 frames per second, each way.
#[derive(Default, Clone, Copy)]
struct TrafficRate {
    rx_bytes_s: f64,
    tx_bytes_s: f64,
    rx_pkt_s: f64,
    tx_pkt_s: f64,
}

/// A parameter popped into its own always-pinnable window: a meter for numeric
/// values, or a red/green indicator light for booleans (chosen by value type).
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

/// An address book loaded from a file, held until the user chooses how to apply it.
struct ImportPending {
    book: AddressBook,
    source: String,
    providers: usize,
    folders: usize,
}

/// Settings that affect how the provider tree is rendered, bundled so they can
/// be threaded through the recursive render functions.
struct RenderOpts {
    pulse_ms: u64,
    show_descriptions: bool,
    order_by: OrderBy,
    matrix_targets_on_top: bool,
    /// Whether value/route/invoke controls are interactive this frame. False when
    /// the safety lock is on and the modifier (Ctrl) isn't held.
    armed: bool,
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
    /// A loaded-but-not-yet-applied address book awaiting a merge/replace choice.
    import: Option<ImportPending>,
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
    /// Screen position to anchor the About window to on the frame it opens, so it
    /// appears next to the About button (where the user's focus is) rather than at
    /// a stale/default corner. Consumed (set to `None`) once applied.
    about_anchor: Option<egui::Pos2>,
    /// Theme currently applied to the egui context (`None` until the first frame
    /// applies it). Re-applied whenever it differs from `settings.dark_mode`.
    applied_dark: Option<bool>,
    /// Safety lock runtime state: when true, value/route/invoke controls are
    /// locked. Initialised from `settings.lock_on_startup`; toggled via the padlock.
    locked: bool,
    /// Shared per-provider connections (desktop + web viewers).
    hubs: HubRegistry,
    /// Provider list exposed to the web server (kept in sync with the book).
    catalog: server::Catalog,
    /// Running web server, when server mode is enabled.
    server: Option<ServerHandle>,
    /// Latest result of the GitHub update check (shown in the menu bar / About).
    update: crate::update::UpdateStatus,
    /// Receiver for an in-flight update check (drained each frame).
    update_rx: Option<std::sync::mpsc::Receiver<crate::update::UpdateStatus>>,
}

/// The project's warm-orange brand accent.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(217, 119, 43);
/// Amber used to mark an optimistic (locally-set, unconfirmed) parameter value.
const PENDING_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 160, 30);

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

/// Non-loopback IPv4 interfaces as `(name, ip)`, for the bind dropdown.
fn list_nics() -> Vec<(String, std::net::Ipv4Addr)> {
    let mut out = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for i in ifaces {
            if i.is_loopback() {
                continue;
            }
            if let std::net::IpAddr::V4(v4) = i.ip() {
                out.push((i.name.clone(), v4));
            }
        }
    }
    out
}

/// A concrete IP to advertise in the URL/QR: the bound IP if specific, else the
/// first non-loopback interface.
fn display_ip(bind: &str, nics: &[(String, std::net::Ipv4Addr)]) -> Option<std::net::Ipv4Addr> {
    if let Ok(v4) = bind.parse::<std::net::Ipv4Addr>() {
        if !v4.is_unspecified() {
            return Some(v4);
        }
    }
    nics.first().map(|(_, ip)| *ip)
}

/// The browser URL for the web UI (token included unless open-LAN).
fn web_url(ip: std::net::Ipv4Addr, port: u16, token: &str, open_lan: bool) -> String {
    if open_lan {
        format!("http://{ip}:{port}/")
    } else {
        format!("http://{ip}:{port}/?token={token}")
    }
}

/// Paint a QR code of `data` (4px modules, with a quiet zone).
fn draw_qr(ui: &mut egui::Ui, data: &str) {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        return;
    };
    let width = code.width();
    let colors = code.to_colors();
    let module = 4.0_f32;
    let quiet = 4.0_f32;
    let side = (width as f32 + 2.0 * quiet) * module;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, egui::Color32::WHITE);
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == qrcode::Color::Dark {
                let px = rect.min.x + (quiet + x as f32) * module;
                let py = rect.min.y + (quiet + y as f32) * module;
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(px, py), egui::vec2(module, module)),
                    0.0,
                    egui::Color32::BLACK,
                );
            }
        }
    }
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
        let locked = settings.lock_on_startup;
        let hubs = HubRegistry::new(rt.handle().clone(), cc.egui_ctx.clone());
        let mut app = App {
            rt,
            book,
            sessions: HashMap::new(),
            active: None,
            add: AddDialog::default(),
            folder_dialog: FolderDialog::default(),
            import: None,
            status_line: String::new(),
            provider_filter: String::new(),
            settings,
            show_options: false,
            log: Vec::new(),
            show_log: false,
            discovery: None,
            show_discovery: false,
            show_about: false,
            about_anchor: None,
            applied_dark: None,
            locked,
            hubs,
            catalog: Arc::new(Mutex::new(server::CatalogData::default())),
            server: None,
            update: crate::update::UpdateStatus::Idle,
            update_rx: None,
        };
        // Resume debug logging if it was on at last shutdown.
        if app.settings.debug_logging {
            if let Err(e) = crate::debug_log::set_enabled(true) {
                app.status_line = format!("debug log: {e}");
                app.settings.debug_logging = false;
            }
        }
        // The theme is applied from within `ui()` (eframe overrides visuals set
        // here during construction, which is why the startup theme didn't stick).
        app.apply_startup_mode(&cc.egui_ctx.clone());
        app.sync_server(); // resume server mode if it was enabled at last shutdown
        app.maybe_check_for_updates(false); // once-per-day GitHub release check
        app
    }

    /// Kick off a GitHub release check if enabled. `force` (the About dialog's
    /// "Check now") bypasses both the opt-out and the 24h throttle.
    fn maybe_check_for_updates(&mut self, force: bool) {
        if self.update_rx.is_some() {
            return; // a check is already in flight
        }
        if !force {
            if !self.settings.check_for_updates {
                return;
            }
            const DAY: u64 = 24 * 60 * 60;
            if now_unix().saturating_sub(self.settings.last_update_check) < DAY {
                return;
            }
        }
        self.settings.last_update_check = now_unix();
        let _ = self.settings.save();
        self.update = crate::update::UpdateStatus::Checking;
        // `EMBERVIEWER_UPDATE_AS_VERSION` pretends the app is an older version, so
        // the "update available" UI can be exercised without shipping a release.
        let current = std::env::var("EMBERVIEWER_UPDATE_AS_VERSION")
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
        self.update_rx = Some(crate::update::spawn_check(current));
    }

    /// Drain an in-flight update check (called each frame).
    fn poll_update_check(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.update_rx {
            if let Ok(status) = rx.try_recv() {
                self.update = status;
                self.update_rx = None;
                ctx.request_repaint();
            }
        }
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
                label_retry: HashMap::new(),
                signal_params: None,
                string_edit: None,
                pending: HashSet::new(),
                flash_until: 0.0,
                traffic_prev: ember_net::TrafficSnapshot::default(),
                traffic_t: 0.0,
                rate: TrafficRate::default(),
            },
        );
        self.active = Some(id);
        self.remember_session();
    }

    /// Rebuild the catalog the web server exposes from the address book: the flat
    /// provider list (for id→addr lookup) and the folder tree (for the left pane).
    fn refresh_catalog(&self) {
        let providers: Vec<WireProvider> = self
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
        let tree: Vec<WireNode> = self.book.root().children.iter().map(node_to_wire).collect();
        let mut c = self.catalog.lock().unwrap();
        c.providers = providers;
        c.tree = tree;
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
                    bind: self.settings.server_bind.clone(),
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
                    NetEvent::Document { roots, .. } => {
                        // Snapshot logged params' values, merge, then log changes.
                        let snaps: Vec<(Vec<u32>, Option<Value>)> = session
                            .logged
                            .iter()
                            .map(|p| (p.clone(), session.tree.get(p).and_then(|e| e.value.clone())))
                            .collect();
                        for root in roots.iter() {
                            session.tree.merge(root.clone());
                        }
                        // A value update from the provider is authoritative: clear
                        // the optimistic "pending" mark on those paths.
                        for p in session.tree.take_value_updates() {
                            session.pending.remove(&p);
                        }
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
                    NetEvent::Retargeted => {
                        // The endpoint changed: drop the old device's tree and wait
                        // for the new one. Keep `subscribed` - the hub replays
                        // subscriptions to the new endpoint, so the ref-counts stay
                        // consistent.
                        session.status = Status::Connecting;
                        session.tree = TreeModel::new();
                        session.pending.clear();
                    }
                    NetEvent::Disconnected(reason) => {
                        session.status =
                            Status::Disconnected(reason.unwrap_or_else(|| "closed".into()));
                        if clear_on_disconnect {
                            session.tree = TreeModel::new();
                            session.subscribed.clear();
                            session.pending.clear();
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

    /// Whether value/route/invoke controls accept input this frame. With the
    /// safety lock off they always do; with it on, only while Ctrl is held.
    fn edits_armed(&self) -> bool {
        !self.locked
    }

    /// Sample each connection's byte/frame counters and update its smoothed
    /// TX/RX rate about once a second. Schedules a repaint so the figure stays
    /// live and decays to zero when traffic stops.
    fn update_traffic(&mut self, now: f64, ctx: &egui::Context) {
        let mut any_active = false;
        for s in self.sessions.values_mut() {
            if matches!(s.status, Status::Connected | Status::Connecting) {
                any_active = true;
            }
            let dt = now - s.traffic_t;
            if dt >= 1.0 {
                let cur = s.handle.traffic();
                if s.traffic_t > 0.0 {
                    let rate = |c: u64, p: u64| c.saturating_sub(p) as f64 / dt;
                    s.rate = TrafficRate {
                        rx_bytes_s: rate(cur.rx_bytes, s.traffic_prev.rx_bytes),
                        tx_bytes_s: rate(cur.tx_bytes, s.traffic_prev.tx_bytes),
                        rx_pkt_s: rate(cur.rx_frames, s.traffic_prev.rx_frames),
                        tx_pkt_s: rate(cur.tx_frames, s.traffic_prev.tx_frames),
                    };
                }
                s.traffic_prev = cur;
                s.traffic_t = now;
            }
        }
        if any_active {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    /// Save the address book to a user-chosen JSON file (to share with colleagues).
    fn export_address_book(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export address book")
            .add_filter("JSON", &["json"])
            .set_file_name("emberviewer-address-book.json")
            .save_file()
        else {
            return;
        };
        match self.book.save_to(&path) {
            Ok(()) => self.status_line = format!("exported address book to {}", path.display()),
            Err(e) => self.status_line = format!("export failed: {e}"),
        }
    }

    /// Pick a JSON file and stage it for import; the user then chooses merge or
    /// replace (so an import never silently overwrites a carefully-built book).
    fn import_address_book(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import address book")
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        match AddressBook::load_from(&path) {
            Ok(book) => {
                let (providers, folders) = book.counts();
                let source = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.import = Some(ImportPending {
                    book,
                    source,
                    providers,
                    folders,
                });
            }
            Err(e) => self.status_line = format!("import failed: {e}"),
        }
    }

    /// Persist the address book after an import, reporting the outcome.
    fn save_book_after_import(&mut self, what: String) {
        match self.book.save() {
            Ok(()) => self.status_line = what,
            Err(e) => self.status_line = format!("{what}, but saving failed: {e}"),
        }
    }

    /// Modal asking whether a staged import should merge into or replace the book.
    fn import_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.import else {
            return;
        };
        let (cur_p, cur_f) = self.book.counts();
        let summary = format!(
            "“{}” has {} provider(s) in {} folder(s).\n\
             Your current book has {} provider(s) in {} folder(s).",
            pending.source, pending.providers, pending.folders, cur_p, cur_f
        );
        let mut choice: Option<bool> = None; // Some(true)=merge, Some(false)=replace
        let mut cancel = false;
        egui::Window::new("Import address book")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(summary);
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Merge keeps your entries and adds the imported ones. \
                         Replace discards your current book.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Merge")
                        .on_hover_text("Add the imported entries to your current book")
                        .clicked()
                    {
                        choice = Some(true);
                    }
                    if ui
                        .button(
                            egui::RichText::new("Replace")
                                .color(egui::Color32::from_rgb(200, 80, 60)),
                        )
                        .on_hover_text("Discard your current book and use the imported one")
                        .clicked()
                    {
                        choice = Some(false);
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.import = None;
            return;
        }
        let Some(merge) = choice else {
            return;
        };
        let pending = self.import.take().expect("pending import present");
        if merge {
            let nodes = pending.book.root().children.clone();
            self.book.graft(AddressBook::ROOT_ID, &nodes);
            self.save_book_after_import(format!(
                "merged {} provider(s) from {}",
                pending.providers, pending.source
            ));
        } else {
            self.book = pending.book;
            self.save_book_after_import(format!("replaced address book from {}", pending.source));
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

/// Open the folder containing `path` in the OS file manager (Explorer / Finder /
/// the desktop's default). Best-effort: failures are silently ignored.
fn open_log_folder(path: &str) {
    use std::path::{Path, PathBuf};
    let p = Path::new(path);
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    // Make absolute without canonicalising (Windows \\?\ paths confuse Explorer).
    let dir = if dir.is_absolute() {
        dir
    } else {
        std::env::current_dir().map(|c| c.join(&dir)).unwrap_or(dir)
    };
    #[cfg(target_os = "windows")]
    let prog = "explorer";
    #[cfg(target_os = "macos")]
    let prog = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let prog = "xdg-open";
    let _ = std::process::Command::new(prog).arg(&dir).spawn();
}

/// Status-bar traffic readout: bit rate and packet rate each way.
fn traffic_text(r: &TrafficRate) -> String {
    format!(
        "RX {} ({:.0} pkt/s) · TX {} ({:.0} pkt/s)",
        fmt_bitrate(r.rx_bytes_s),
        r.rx_pkt_s,
        fmt_bitrate(r.tx_bytes_s),
        r.tx_pkt_s,
    )
}

/// A bytes-per-second value as a human bit rate.
fn fmt_bitrate(bytes_s: f64) -> String {
    let bits = bytes_s * 8.0;
    if bits >= 1.0e6 {
        format!("{:.1} Mbit/s", bits / 1.0e6)
    } else if bits >= 1.0e3 {
        format!("{:.0} kbit/s", bits / 1.0e3)
    } else {
        format!("{bits:.0} bit/s")
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Current wall-clock time as `HH:MM:SS` (UTC).
fn timestamp() -> String {
    let secs = now_unix();
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
        self.update_traffic(ctx.input(|i| i.time), &ctx);
        self.process_pulses(&ctx);
        self.poll_discovery(&ctx);
        self.poll_update_check(&ctx);
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
                ui.menu_button("Address book", |ui| {
                    if ui
                        .button("Export…")
                        .on_hover_text("Save the address book to a JSON file to share")
                        .clicked()
                    {
                        self.export_address_book();
                        ui.close();
                    }
                    if ui
                        .button("Import…")
                        .on_hover_text("Load a JSON file, then choose to merge or replace")
                        .clicked()
                    {
                        self.import_address_book();
                        ui.close();
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let about_btn = ui.selectable_label(self.show_about, "About");
                    if about_btn.clicked() {
                        self.show_about = !self.show_about;
                        // Anchor the window just under the button so it opens where
                        // the user just clicked (their eyes are already there).
                        if self.show_about {
                            self.about_anchor = Some(about_btn.rect.right_bottom());
                        }
                    }
                    // Accent "Update available" chip (just left of About) when a
                    // newer release exists; clicking it opens the release page.
                    if let crate::update::UpdateStatus::Available { version, url } =
                        self.update.clone()
                    {
                        ui.separator();
                        let resp = ui
                            .horizontal(|ui| {
                                paint_dot(ui, ACCENT);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("Update available")
                                            .color(ACCENT)
                                            .strong(),
                                    )
                                    .sense(egui::Sense::click()),
                                )
                            })
                            .inner
                            .on_hover_text(format!(
                                "emberviewer {version} is available - click to download"
                            ));
                        if resp.clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        }
                    }
                    ui.separator();
                    // Safety padlock: lock/arm the value/route/invoke controls.
                    let now = ui.input(|i| i.time);
                    let flash = self
                        .active
                        .and_then(|id| self.sessions.get(&id))
                        .map(|s| s.flash_until)
                        .unwrap_or(0.0);
                    lock_toggle(ui, &mut self.locked, now, flash);
                });
            });
        });

        self.sidebar(ui, &ctx);
        self.add_dialog(&ctx);
        self.folder_dialog(&ctx);
        self.import_dialog(&ctx);
        self.options_window(&ctx);
        self.about_window(&ctx);
        self.signal_params_window(&ctx);
        self.string_edit_window(&ctx);
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
                        if matches!(s.status, Status::Connected) {
                            ui.separator();
                            ui.label(traffic_text(&s.rate)).on_hover_text(
                                "Traffic on this device's connection (shared by all viewers).\n\
                                 RX = received from the device, TX = sent to it.",
                            );
                        }
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
                            // Stable id so focus survives the ✖ button appearing
                            // once text is entered (which would otherwise shift the
                            // auto-generated id and drop keyboard focus mid-typing).
                            .id_salt("provider-filter")
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
                    // Only while dragging a folder/provider, offer a target to move
                    // it out to the top level - shown as a highlighted drop area
                    // rather than always-present text, and hidden otherwise.
                    if egui::DragAndDrop::has_payload_of_type::<DragPayload>(ui.ctx()) {
                        ui.add_space(8.0);
                        let frame = egui::Frame::default()
                            .inner_margin(8.0)
                            .corner_radius(6.0)
                            .stroke(egui::Stroke::new(1.0, ACCENT))
                            .fill(ACCENT.gamma_multiply(0.10));
                        let (_, payload) = ui.dnd_drop_zone::<DragPayload, ()>(frame, |ui| {
                            ui.set_width(ui.available_width());
                            ui.vertical_centered(|ui| {
                                ui.label(egui::RichText::new("Move to top level").color(ACCENT));
                            });
                        });
                        if let Some(p) = payload {
                            action = Some(SidebarAction::Move {
                                node: p.0,
                                into: AddressBook::ROOT_ID,
                            });
                        }
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
                // Drop a dragged provider/folder onto this folder's row to move it
                // inside. While a drag is in progress, make the whole header row a
                // drop target with a highlight, instead of relying on the bare
                // (narrow, unhighlighted) collapsing-header response.
                if egui::DragAndDrop::has_payload_of_type::<DragPayload>(ui.ctx()) {
                    let row =
                        egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), hr.rect.y_range());
                    let drop = ui.interact(
                        row,
                        ui.make_persistent_id(("folder-drop", folder.id)),
                        egui::Sense::hover(),
                    );
                    if drop.contains_pointer() {
                        ui.painter()
                            .rect_filled(row, 4.0, ACCENT.gamma_multiply(0.22));
                    }
                    if let Some(p) = drop.dnd_release_payload::<DragPayload>() {
                        if p.0 != folder.id {
                            *action = Some(SidebarAction::Move {
                                node: p.0,
                                into: folder.id,
                            });
                        }
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
                let mut submit = false;
                egui::Grid::new("addgrid").num_columns(2).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.add.name);
                    ui.end_row();
                    ui.label("Host");
                    let host = ui.text_edit_singleline(&mut self.add.host);
                    ui.end_row();
                    ui.label("Port");
                    let port = ui.text_edit_singleline(&mut self.add.port);
                    ui.end_row();
                    // Enter in the host or port field commits, like the Add/Save button.
                    submit = (host.lost_focus() || port.lost_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let valid = !self.add.host.trim().is_empty();
                    let save_label = if editing.is_some() { "Save" } else { "Add" };
                    let clicked = ui
                        .add_enabled(valid, egui::Button::new(save_label))
                        .clicked();
                    if (clicked || submit) && valid {
                        let port = self.add.port.trim().parse().unwrap_or(DEFAULT_PORT);
                        let host = self.add.host.trim().to_string();
                        let name = if self.add.name.trim().is_empty() {
                            host.clone()
                        } else {
                            self.add.name.clone()
                        };
                        match editing {
                            Some(id) => {
                                let new_addr = format!("{host}:{port}");
                                let addr_changed = self
                                    .book
                                    .find_provider(id)
                                    .is_some_and(|p| format!("{}:{}", p.host, p.port) != new_addr);
                                self.book
                                    .update_provider(id, name.clone(), host, port, None);
                                if let Some(s) = self.sessions.get_mut(&id) {
                                    s.name = name;
                                    if addr_changed {
                                        s.addr = new_addr.clone();
                                    }
                                }
                                if addr_changed {
                                    // Move the shared connection (this desktop and
                                    // any browsers) to the new endpoint in place, so
                                    // every viewer of this provider stays on the same
                                    // target. The hub emits Retargeted, which resets
                                    // each viewer's now-stale tree.
                                    self.hubs.retarget(id, new_addr);
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
                    ui.horizontal(|ui| {
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.settings.log_file)
                                    .hint_text("optional path"),
                            )
                            .changed();
                        let has_path = !self.settings.log_file.trim().is_empty();
                        if ui
                            .add_enabled(has_path, egui::Button::new("Open folder"))
                            .on_hover_text("Show the log file's folder in your file manager")
                            .clicked()
                        {
                            open_log_folder(self.settings.log_file.trim());
                        }
                    });
                    ui.end_row();
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.settings.debug_logging, "Enable debug log")
                        .on_hover_text(
                            "Capture connection events and the raw Ember+ frames sent and \
                             received to a file, for diagnosing devices that misbehave in \
                             the viewer. Off by default; the file can grow large.",
                        )
                        .changed()
                    {
                        if let Err(e) = crate::debug_log::set_enabled(self.settings.debug_logging) {
                            self.status_line = format!("debug log: {e}");
                            self.settings.debug_logging = false;
                        }
                        changed = true;
                    }
                    if ui
                        .button("Open log folder")
                        .on_hover_text("Show the debug-log folder in your file manager")
                        .clicked()
                    {
                        if let Some(dir) = crate::debug_log::logs_dir() {
                            let _ = std::fs::create_dir_all(&dir);
                            // `open_log_folder` opens a path's parent folder.
                            open_log_folder(&dir.join("debug.log").to_string_lossy());
                        }
                    }
                });
                if self.settings.debug_logging {
                    if let Some(p) = crate::debug_log::current_path() {
                        ui.weak(format!("→ {}", p.display()));
                    }
                }
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
                        &mut self.settings.check_for_updates,
                        "Check for updates on startup",
                    )
                    .on_hover_text(
                        "Once a day on launch, contacts GitHub to see if a newer release \
                         exists. Sends nothing about your devices.",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.settings.matrix_targets_on_top,
                        "Matrix: targets on top (else sources on top)",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.settings.lock_on_startup,
                        "Start with controls locked (safety)",
                    )
                    .on_hover_text(
                        "The padlock in the top bar locks value/route/invoke controls against\n\
                         accidental changes. This sets whether it starts locked on launch;\n\
                         toggle the padlock any time during a session.",
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
                    ui.label("Bind to");
                    let nics = list_nics();
                    let current = if self.settings.server_bind == "0.0.0.0" {
                        "All interfaces".to_string()
                    } else {
                        self.settings.server_bind.clone()
                    };
                    egui::ComboBox::from_id_salt("bind")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(
                                    self.settings.server_bind == "0.0.0.0",
                                    "All interfaces",
                                )
                                .clicked()
                            {
                                self.settings.server_bind = "0.0.0.0".into();
                                changed = true;
                                do_restart = true;
                            }
                            for (name, ip) in &nics {
                                let s = ip.to_string();
                                if ui
                                    .selectable_label(
                                        self.settings.server_bind == s,
                                        format!("{name} ({ip})"),
                                    )
                                    .clicked()
                                {
                                    self.settings.server_bind = s;
                                    changed = true;
                                    do_restart = true;
                                }
                            }
                        });
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
                        "Open on LAN (no token - anyone can control)",
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
                    let nics = list_nics();
                    if let Some(ip) = display_ip(&self.settings.server_bind, &nics) {
                        let url = web_url(
                            ip,
                            srv.bound.port(),
                            &self.settings.server_token,
                            self.settings.server_open_lan,
                        );
                        ui.horizontal(|ui| {
                            ui.hyperlink_to("open web page", &url);
                            if ui.button("Copy URL").clicked() {
                                ui.ctx().copy_text(url.clone());
                            }
                        });
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Scan to open on a phone:")
                                .small()
                                .weak(),
                        );
                        draw_qr(ui, &url);
                    } else if !self.settings.server_open_lan
                        && ui.button("Copy access token").clicked()
                    {
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
        use crate::update::UpdateStatus;
        let mut open = self.show_about;
        let status = self.update.clone();
        let mut check_now = false;
        let mut about = egui::Window::new("About")
            .open(&mut open)
            .resizable(false)
            .collapsible(false);
        // On the frame it opens, snap the window's top-right corner just below the
        // About button. Only for that frame, so the user can still drag it after.
        if let Some(anchor) = self.about_anchor.take() {
            about = about.current_pos(anchor).pivot(egui::Align2::RIGHT_TOP);
        }
        about.show(ctx, |ui| {
            ui.heading(egui::RichText::new("emberviewer").color(ACCENT));
            ui.label("A cross-platform Ember+ viewer");
            ui.add_space(4.0);
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label("An open replacement for Lawo's EmberPlusView.");
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);

            // Update status + a manual "Check for updates".
            match &status {
                UpdateStatus::Available { version, url } => {
                    ui.horizontal(|ui| {
                        paint_dot(ui, ACCENT);
                        ui.label(
                            egui::RichText::new(format!("Update available: {version}"))
                                .color(ACCENT)
                                .strong(),
                        );
                    });
                    ui.hyperlink_to("Download the latest release", url);
                }
                UpdateStatus::UpToDate => {
                    ui.label("You're on the latest version.");
                }
                UpdateStatus::Checking => {
                    ui.label("Checking for updates…");
                }
                UpdateStatus::Failed(e) => {
                    ui.label(egui::RichText::new("Couldn't check for updates.").weak())
                        .on_hover_text(e.as_str());
                }
                UpdateStatus::Idle => {}
            }
            let checking = matches!(status, UpdateStatus::Checking);
            if ui
                .add_enabled(!checking, egui::Button::new("Check for updates"))
                .clicked()
            {
                check_now = true;
            }

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
        if check_now {
            self.maybe_check_for_updates(true);
        }
    }

    /// Multi-line string editor/viewer window (opened from a string parameter's
    /// `Edit…`/`View…` button); applies edits via `SetValue`.
    fn string_edit_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.active else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        if let Some((path, value)) = string_edit_window(ctx, &mut session.string_edit) {
            // Optimistically reflect the applied value: many SDP/string params are
            // write-mostly and never echo the value back, which would otherwise
            // leave the row blank after Apply. A real device update (which carries
            // a value) still overrides this.
            if let Some(e) = session.tree.entries.get_mut(&path) {
                e.value = Some(value.clone());
            }
            // Mark optimistic until the provider's value update confirms it.
            session.pending.insert(path.clone());
            session
                .handle
                .send(NetCommand::SetValue(path.clone(), value));
            // Re-read so the device's authoritative value wins: if it rejected a
            // malformed SDP and blanked its own value, we show that truth.
            session.handle.send(NetCommand::RefreshValue(path));
        }
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
            armed: self.edits_armed(),
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
                        ui.label(egui::RichText::new(meter_readout(&entry, v)).small());
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
        // Device identity, shown on every pop-out so several stay distinguishable.
        let dev_name = session.name.clone();
        let dev_addr = session.addr.clone();
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
            // A boolean pops out as an indicator light; anything numeric as a meter.
            let bool_value = match &entry.value {
                Some(Value::Boolean(b)) => Some(*b),
                _ => None,
            };
            let range = if bool_value.is_none() {
                meter_range(&entry, &mut session.meter_range)
            } else {
                (0.0, 1.0)
            };
            let value = entry.value.as_ref().and_then(value_f64);
            let title = entry.label();
            let path_str = path
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(".");
            // Flank the meter with its identity so several open pop-outs stay
            // distinguishable: device (name · host:port) reads up the left,
            // parameter (name · path · description) reads up the right.
            let left_text = format!("{dev_name}  ·  {dev_addr}");
            let mut right_text = format!("{title}  ·  {path_str}");
            if let Some(d) = entry.description.as_deref().filter(|d| !d.is_empty()) {
                right_text.push_str("  ·  ");
                right_text.push_str(d);
            }
            let vp_id = egui::ViewportId::from_hash_of(("popmeter", id, &path));
            // Borderless floating window: no OS title bar / min-max-close chrome.
            // We provide drag-to-move and a right-click menu (close / pin) instead.
            let mut builder = egui::ViewportBuilder::default()
                .with_title(format!("{title} - {dev_name}"))
                .with_decorations(false)
                .with_resizable(true)
                .with_inner_size([150.0, 300.0])
                .with_min_inner_size([104.0, 150.0]);
            if aot {
                builder = builder.with_always_on_top();
            }
            let mut close = false;
            let mut toggle = false;
            ctx.show_viewport_immediate(vp_id, builder, |ctx, _| {
                // A 1px border delineates the borderless window against the desktop.
                let frame = egui::Frame::NONE
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin::same(6))
                    .stroke(ctx.style().visuals.window_stroke());
                egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                    let color = ui.visuals().text_color();
                    let full_h = ui.available_height();
                    let readout_h = if value.is_some() || bool_value.is_some() {
                        22.0
                    } else {
                        0.0
                    };
                    let mh = (full_h - readout_h - 4.0).max(50.0);
                    // Centre a fixed-width column (labels + meter) as one block so
                    // the meter and the readout beneath it stay aligned at any
                    // window width - `vertical_centered` alone would let the meter
                    // row span full width (left-aligned) while centring the readout.
                    const SIDE_W: f32 = 16.0;
                    const METER_W: f32 = 40.0;
                    const GAP: f32 = 2.0;
                    let content_w = SIDE_W * 2.0 + METER_W + GAP * 2.0;
                    let off = ((ui.available_width() - content_w) * 0.5).max(0.0);
                    ui.horizontal(|ui| {
                        ui.add_space(off);
                        ui.vertical(|ui| {
                            ui.set_width(content_w);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = GAP;
                                meter_side_label(ui, SIDE_W, mh, &left_text, color);
                                if let Some(on) = bool_value {
                                    draw_indicator(ui, Some(on), METER_W, mh);
                                } else {
                                    draw_vmeter(ui, value, range, METER_W, mh);
                                }
                                meter_side_label(ui, SIDE_W, mh, &right_text, color);
                            });
                            if let Some(v) = value {
                                ui.vertical_centered(|ui| {
                                    ui.label(meter_readout(&entry, v));
                                });
                            } else if let Some(on) = bool_value {
                                ui.vertical_centered(|ui| {
                                    ui.label(if on { "true" } else { "false" });
                                });
                            }
                        });
                    });
                    // Borderless interaction: the whole window is a drag handle (move
                    // it like a title bar) and a right-click target (close / pin), with
                    // a hover tooltip carrying the full, untruncated identity.
                    let bg = ui.interact(
                        ui.max_rect(),
                        ui.id().with("winbg"),
                        egui::Sense::click_and_drag(),
                    );
                    if bg.drag_started_by(egui::PointerButton::Primary) {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    let mut tip = format!("{title}\n{dev_name}  ·  {dev_addr}\n{path_str}");
                    if let Some(d) = entry.description.as_deref().filter(|d| !d.is_empty()) {
                        tip.push('\n');
                        tip.push_str(d);
                    }
                    bg.on_hover_text(tip).context_menu(|ui| {
                        ui.label(egui::RichText::new(&title).strong());
                        ui.label(
                            egui::RichText::new(format!("{dev_name}  ·  {dev_addr}"))
                                .small()
                                .weak(),
                        );
                        ui.separator();
                        ui.label(
                            egui::RichText::new("Drag the meter to move it")
                                .small()
                                .weak(),
                        );
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

                    let area = ui.max_rect();
                    // Faint × (top-right) to close - drawn, not a font glyph (those
                    // tofu in the default font). Brightens on hover.
                    let x_rect = egui::Rect::from_min_size(
                        egui::pos2(area.right() - 17.0, area.top() + 1.0),
                        egui::vec2(16.0, 16.0),
                    );
                    let x_resp = ui
                        .interact(x_rect, ui.id().with("close"), egui::Sense::click())
                        .on_hover_text("Close");
                    let x_col = if x_resp.hovered() {
                        egui::Color32::from_rgb(220, 90, 80)
                    } else {
                        ui.visuals().weak_text_color().gamma_multiply(0.55)
                    };
                    let xc = x_rect.center();
                    let xs = egui::Stroke::new(1.5, x_col);
                    ui.painter()
                        .line_segment([xc + egui::vec2(-3.5, -3.5), xc + egui::vec2(3.5, 3.5)], xs);
                    ui.painter()
                        .line_segment([xc + egui::vec2(3.5, -3.5), xc + egui::vec2(-3.5, 3.5)], xs);
                    if x_resp.clicked() {
                        close = true;
                    }

                    // Bottom-right resize grip - borderless windows have no OS
                    // resize border, so drag this to set the window's inner size.
                    let g_rect = egui::Rect::from_min_size(
                        area.right_bottom() - egui::vec2(14.0, 14.0),
                        egui::vec2(14.0, 14.0),
                    );
                    let g_resp = ui
                        .interact(g_rect, ui.id().with("grip"), egui::Sense::drag())
                        .on_hover_cursor(egui::CursorIcon::ResizeNwSe);
                    if g_resp.dragged() {
                        let cur = ui.ctx().input(|i| i.screen_rect().size());
                        let new = (cur + g_resp.drag_delta()).max(egui::vec2(104.0, 150.0));
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::InnerSize(new));
                    }
                    let g_col = ui
                        .visuals()
                        .weak_text_color()
                        .gamma_multiply(if g_resp.hovered() { 1.0 } else { 0.6 });
                    for off in [2.0, 6.0, 10.0] {
                        ui.painter().line_segment(
                            [
                                egui::pos2(g_rect.right() - off, g_rect.bottom() - 1.5),
                                egui::pos2(g_rect.right() - 1.5, g_rect.bottom() - off),
                            ],
                            egui::Stroke::new(1.0, g_col),
                        );
                    }
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
        // Compute before borrowing the session (which borrows `self.sessions`).
        let armed = self.edits_armed();
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };

        ui.horizontal(|ui| {
            ui.heading(&session.name);
            // The host:port: readable secondary text (not the very-faint `weak`),
            // kept subordinate to the heading by colour rather than tiny size.
            ui.label(
                egui::RichText::new(&session.addr)
                    .color(ui.visuals().widgets.inactive.fg_stroke.color),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !session.filter.is_empty() && ui.button("✖").clicked() {
                    session.filter.clear();
                }
                ui.add(
                    egui::TextEdit::singleline(&mut session.filter)
                        // Stable id so focus survives the ✖ button appearing (see
                        // the provider filter above).
                        .id_salt("tree-filter")
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
            armed,
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
    let Some(entry) = session.tree.get(path) else {
        return;
    };
    let eff_online = online && entry.is_online;

    if entry.kind.is_expandable() {
        let id = ui.make_persistent_id(("node", path));
        // Captured before `show_header` borrows `ui` (the borrow lasts until
        // `header.body`), so the matrix label fetch below can use them.
        let now = ui.input(|i| i.time);
        let ctx = ui.ctx().clone();
        // Snapshot only the cheap fields needed across the `&mut session` borrows
        // below. The matrix / function detail (whose label & connection maps can be
        // enormous - a 5001-target matrix) is deliberately NOT cloned; it is
        // re-borrowed from the tree at each point of use, inside blocks that don't
        // overlap a `&mut session` borrow.
        let heading_text = node_label(entry, opts);
        let identifier = entry.identifier.clone();
        let has_matrix = entry.matrix.is_some();
        let has_function = entry.function.is_some();
        let requested = entry.requested;
        let matrix_fetch = entry
            .matrix
            .as_ref()
            .map(|m| (m.label_paths.clone(), m.params_location.clone()));
        // The (small) resolved per-signal parameter node bases, for a header click.
        let param_axis_paths = entry
            .matrix
            .as_ref()
            .map(|m| (m.param_targets_path.clone(), m.param_sources_path.clone()));
        // `entry` borrow released here (only Copy/owned snapshots survive).

        let mut heading = egui::RichText::new(heading_text);
        if !eff_online {
            heading = heading.weak().italics();
        }
        let mut name_clicked = false;
        let mut header =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, |ui| {
                    // Clicking the node name (not just the triangle) toggles it,
                    // matching the address-book folders.
                    let label = ui
                        .add(egui::Label::new(heading).sense(egui::Sense::click()))
                        .on_hover_text(if eff_online { "" } else { "offline" });
                    label.context_menu(|ui| context_copy(ui, path, &identifier));
                    name_clicked = label.clicked();
                });
        if name_clicked {
            header.toggle();
        }
        let next_open = header.is_open();
        // Eagerly fetch a matrix's directory and label/param sub-trees as soon as
        // the node is *visible* (not only when its grid is open). The label subtree
        // is several getDirectory levels deep (basePath → targets/sources → string
        // params), so kicking it off early - and on every frame the matrix is in
        // view, regardless of expand state - lets the multi-phase fetch finish even
        // on slow devices, instead of stalling if the grid is collapsed mid-fetch.
        if let Some((label_paths, params_location)) = &matrix_fetch {
            if session.label_fetch.insert(path.to_vec()) {
                commands.push(NetCommand::GetMatrixDirectory(path.to_vec()));
            }
            let mut pending = false;
            for base in label_paths {
                // basePath may be absolute or relative-to-parent depending on the
                // provider; fetch each interpretation that points at a real node.
                let cands = crate::model::fetchable_label_bases(&session.tree, path, base);
                for cand in cands {
                    pending |= fetch_label_subtree(session, &cand, now, commands);
                }
            }
            if let Some(ploc) = params_location {
                pending |= fetch_label_subtree(session, ploc, now, commands);
            }
            // Keep ticking while a label fetch is still outstanding, so the retry
            // fires even if nothing else (e.g. a live meter) is repainting.
            if pending {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                    LABEL_FETCH_RETRY_SECS,
                ));
            }
        }
        header.body(|ui| {
            // Matrix grid / function form, when this node is one.
            if has_matrix {
                // The grid is greyed and inert when locked; a click then flashes
                // the padlock to show why nothing routed. The matrix detail is
                // borrowed from the tree only for the paint; `render_matrix` returns
                // the (Copy) click/blocked outcome, so the borrow ends before we
                // touch `&mut session`.
                let (clicked, blocked) = {
                    let entry = session.tree.get(path).expect("matrix entry present");
                    let m = entry.matrix.as_ref().expect("matrix present");
                    lockable(ui, opts.armed, |ui| {
                        crate::matrix_view::render_matrix(
                            ui,
                            entry,
                            m,
                            opts.matrix_targets_on_top,
                            commands,
                        )
                    })
                };
                if blocked {
                    session.flash_until = ui.input(|i| i.time) + LOCK_FLASH_SECS;
                }
                if let Some((is_target, sig)) = clicked {
                    // Open the clicked signal's parameter node (gain/type/…).
                    let base = match &param_axis_paths {
                        Some((t, s)) => {
                            if is_target {
                                t.clone()
                            } else {
                                s.clone()
                            }
                        }
                        None => None,
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
            } else if has_function {
                let blocked = {
                    let entry = session.tree.get(path).expect("function entry present");
                    let f = entry.function.as_ref().expect("function present");
                    lockable(ui, opts.armed, |ui| {
                        render_function(
                            ui,
                            entry,
                            f,
                            &mut session.func_inputs,
                            &mut session.invocations,
                            &mut session.next_invocation_id,
                            &session.tree.invocation_results,
                            commands,
                        );
                    })
                    .1
                };
                if blocked {
                    session.flash_until = ui.input(|i| i.time) + LOCK_FLASH_SECS;
                }
            }
            let children = {
                let entry = session.tree.get(path).expect("node entry present");
                sorted_paths(&session.tree, &entry.children, opts.order_by)
            };
            for child in &children {
                render_entry(ui, session, child, opts, eff_online, row, commands, visible);
            }
            if children.is_empty() && !has_matrix && !has_function {
                ui.weak("…");
            }
        });
        // Lazily request children whenever a node is open but not yet fetched.
        // (`requested` is reset on reconnect, so this also drives re-discovery.)
        session.open.insert(path.to_vec(), next_open);
        if next_open && !requested {
            if let Some(e) = session.tree.entries.get_mut(path) {
                e.requested = true;
            }
            if has_matrix {
                commands.push(NetCommand::GetMatrixDirectory(path.to_vec()));
            } else {
                commands.push(NetCommand::GetDirectory(path.to_vec()));
            }
        }
    } else {
        // A leaf parameter is on screen → eligible for a live subscription. A leaf
        // has no matrix maps, so this clone is cheap (it decouples the entry from
        // `session` so `render_parameter` can take `&mut session` alongside it).
        visible.push(path.to_vec());
        let entry = entry.clone();
        render_parameter(ui, session, &entry, opts, eff_online, row, commands);
    }
}

/// Render a parameter row: label, value, and (if writable) an editor.
/// The shared right-click menu for a parameter row (copy path/value, log changes,
/// pop out the meter). Attached to both the name and the value so either works.
fn param_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    entry: &crate::model::Entry,
    logging: bool,
    meterable: bool,
) {
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
    let is_bool = matches!(entry.value, Some(Value::Boolean(_)));
    let pop_label = if is_bool {
        Some("Pop out boolean")
    } else if meterable {
        Some("Pop out meter")
    } else {
        None
    };
    if let Some(label) = pop_label {
        if ui.button(label).clicked() {
            if !session.popped.iter().any(|p| p.path == entry.path) {
                session.popped.push(PoppedMeter {
                    path: entry.path.clone(),
                    always_on_top: false,
                });
            }
            ui.close();
        }
    }
}

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
    let pending = session.pending.contains(&entry.path);
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
            if pending {
                // Amber dot: value was set locally but not yet confirmed by the
                // provider, so what's shown is optimistic, not authoritative.
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(12.0, ui.spacing().interact_size.y),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .circle_filled(rect.center(), 4.5, PENDING_COLOR);
                resp.on_hover_text("Pending: set locally, awaiting device confirmation");
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
            name.context_menu(|ui| param_menu(ui, session, entry, logging, meterable));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Keep values clear of the (Windows) scrollbar.
                ui.add_space(14.0);
                // Inline mini-meter for numeric params: click to show the big meter,
                // right-click for the same options as the name/value.
                if meterable {
                    let range = meter_range(entry, &mut session.meter_range);
                    let value = entry.value.as_ref().and_then(value_f64);
                    let h = ui.spacing().interact_size.y;
                    let mr = draw_vmeter(ui, value, range, 10.0, h);
                    if mr.clicked() {
                        session.selected = Some(entry.path.clone());
                    }
                    mr.context_menu(|ui| param_menu(ui, session, entry, logging, meterable));
                }
                if entry.param_type == Some(glow::parameter_type::TRIGGER) {
                    let (_, blocked) = lockable(ui, opts.armed, |ui| {
                        if ui.button("Fire").clicked() {
                            commands
                                .push(NetCommand::SetValue(entry.path.clone(), Value::Integer(0)));
                        }
                    });
                    if blocked {
                        session.flash_until = ui.input(|i| i.time) + LOCK_FLASH_SECS;
                    }
                } else if entry.is_writable() {
                    let (_, blocked) = lockable(ui, opts.armed, |ui| {
                        editor(ui, session, entry, opts, commands)
                    });
                    if blocked {
                        session.flash_until = ui.input(|i| i.time) + LOCK_FLASH_SECS;
                    }
                } else if let Some(v) = &entry.value {
                    // Read-only strings get a viewer for long/multi-line values
                    // (e.g. SDPs) that the truncated inline label can't show.
                    if let Value::String(s) = v {
                        if ui
                            .button("View…")
                            .on_hover_text("View in a larger box")
                            .clicked()
                        {
                            session.string_edit = Some(StringEdit::new(entry, s));
                        }
                    }
                    // The value is also a select/right-click target (like the name),
                    // so you can click it to show the meter or pop it out. Strings
                    // keep their line breaks (CR dropped) and wrap, so a multi-line
                    // value grows the row taller instead of running off to the side.
                    let is_string = matches!(v, Value::String(_));
                    let preview = match v {
                        Value::String(s) => clean_multiline(s),
                        _ => display_value(entry, v),
                    };
                    // Only numeric/meterable values can drive the meter; don't
                    // promise a meter for strings and other non-meterable types.
                    let hover = if meterable {
                        "click to show meter · right-click for options"
                    } else {
                        "right-click for options"
                    };
                    let mut text = egui::RichText::new(preview);
                    if pending {
                        text = text.color(PENDING_COLOR);
                    }
                    let mut label = egui::Label::new(text).sense(egui::Sense::click());
                    if is_string {
                        label = label.wrap();
                    }
                    let vresp = ui.add(label).on_hover_text(hover);
                    if vresp.clicked() {
                        session.selected = Some(entry.path.clone());
                    }
                    vresp.context_menu(|ui| param_menu(ui, session, entry, logging, meterable));
                } else {
                    ui.weak("-");
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
        Some(Value::String(s)) => {
            // Pop-out multi-line editor (for SDPs and other long/multi-line
            // values) alongside the inline field for quick single-line edits.
            if ui
                .button("Edit…")
                .on_hover_text("Edit in a larger box")
                .clicked()
            {
                session.string_edit = Some(StringEdit::new(entry, s));
            }
            if s.contains(['\n', '\r']) {
                // A single-line field can't represent newlines (they'd tofu and a
                // commit would mangle the value); edit multi-line strings via the
                // pop-out instead, showing a line-break-preserving preview inline.
                let mut text = egui::RichText::new(clean_multiline(s));
                if session.pending.contains(&entry.path) {
                    text = text.color(PENDING_COLOR);
                }
                ui.add(egui::Label::new(text).wrap());
            } else {
                text_commit_editor(ui, session, entry, commands, |s| {
                    Some(Value::String(s.to_string()))
                });
            }
        }
        None => {
            text_commit_editor(ui, session, entry, commands, |s| {
                Some(Value::String(s.to_string()))
            });
        }
        Some(Value::Octets(_)) => {
            ui.weak("<octets>");
        }
    }
}

/// Eagerly fetch a matrix label sub-tree so source/target names resolve:
/// request the base node, then its `targets`/`sources` children (whose
/// getDirectory returns the label string params).
///
/// A one-shot request isn't enough: embedded devices (e.g. Arkona AT300) drop
/// getDirectory requests issued during the initial discovery burst, so the base
/// node's children would never arrive and the labels stayed blank until the user
/// manually expanded the sub-tree (which re-requested it). [`label_fetch_step`]
/// re-requests any still-childless node, throttled and capped, until it
/// populates. Returns true while any node is still pending (so the caller keeps
/// the UI repainting until the retry fires).
fn fetch_label_subtree(
    session: &mut Session,
    base: &[u32],
    now: f64,
    commands: &mut Vec<NetCommand>,
) -> bool {
    let mut fetch_if_empty = |session: &mut Session, path: &[u32]| -> bool {
        let has_children = session
            .tree
            .get(path)
            .is_some_and(|e| !e.children.is_empty());
        let step = label_fetch_step(session.label_retry.get(path).copied(), has_children, now);
        if let Some(new_state) = step.new_state {
            session.label_retry.insert(path.to_vec(), new_state);
        }
        if step.request {
            commands.push(NetCommand::GetDirectory(path.to_vec()));
        }
        step.pending
    };

    // The base usually exists as a childless stub (created by the matrix's parent
    // fetch); keep asking until its targets/sources children arrive.
    let mut pending = fetch_if_empty(session, base);
    // Once the base's children (targets/sources) are known, fetch each of them
    // too (their getDirectory returns the label string params).
    let children = session
        .tree
        .get(base)
        .map(|e| e.children.clone())
        .unwrap_or_default();
    for child in children {
        pending |= fetch_if_empty(session, &child);
    }
    pending
}

/// Draw a `w`×`h` column holding `text` rotated 90° so it reads bottom-to-top,
/// clipped to the column. Used to flank a pop-out meter with its device and
/// parameter identity (the full strings are in the window's hover tooltip).
fn meter_side_label(ui: &mut egui::Ui, w: f32, h: f32, text: &str, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.0), color);
    let mut shape = egui::epaint::TextShape::new(
        // Start the baseline near the bottom, centred across the column width;
        // the string's start (most important info) sits at the bottom and any
        // overflow clips off the top.
        egui::pos2(rect.center().x - galley.size().y / 2.0, rect.bottom() - 2.0),
        galley,
        color,
    );
    shape.angle = -std::f32::consts::FRAC_PI_2;
    ui.painter_at(rect).add(shape);
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

/// Convert an address-book node into its wire form (folders + providers) for the
/// browser's left pane.
fn node_to_wire(node: &Node) -> WireNode {
    match node {
        Node::Provider(p) => WireNode::Provider(WireProvider {
            id: p.id,
            name: p.name.clone(),
            host: p.host.clone(),
            port: p.port,
        }),
        Node::Folder(f) => WireNode::Folder {
            name: f.name.clone(),
            children: f.children.iter().map(node_to_wire).collect(),
        },
    }
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
        (true, Some(d)) if !d.is_empty() => format!("{base} - {d}"),
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
