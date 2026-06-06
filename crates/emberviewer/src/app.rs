//! The eframe application: address-book sidebar + provider tree browser.
//!
//! egui 0.34 deprecated the `SidePanel`/`TopBottomPanel` aliases in favour of a
//! unified `Panel` API; the aliases still work, so we keep them for now and
//! migrate when that API settles.
#![allow(deprecated)]

use std::collections::{HashMap, HashSet};

use ember_proto::glow::{self, Value};

use crate::address_book::{AddressBook, Id, Node, DEFAULT_PORT};
use crate::model::{format_value, TreeModel};
use crate::net::{ConnectionHandle, NetCommand, NetEvent};
use crate::settings::{OrderBy, Settings, StartupMode};

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

/// An action requested from the sidebar (often via a right-click menu).
enum SidebarAction {
    Open(Id),
    Disconnect(Id),
    Edit(Id),
    Remove(Id),
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
    status_line: String,
    /// Filter text for the providers sidebar.
    provider_filter: String,
    settings: Settings,
    show_options: bool,
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
        let mut app = App {
            rt,
            book,
            sessions: HashMap::new(),
            active: None,
            add: AddDialog::default(),
            status_line: String::new(),
            provider_filter: String::new(),
            settings,
            show_options: false,
        };
        app.apply_startup_mode(&cc.egui_ctx.clone());
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

    /// Close a session: ask the task to stop and drop its state.
    fn disconnect(&mut self, id: Id) {
        if let Some(session) = self.sessions.remove(&id) {
            session.handle.send(NetCommand::Disconnect);
        }
        if self.active == Some(id) {
            self.active = self.sessions.keys().copied().next();
        }
        self.remember_session();
    }

    fn connect(&mut self, id: Id, ctx: &egui::Context) {
        let Some(provider) = self.book.find_provider(id).cloned() else {
            return;
        };
        let addr = format!("{}:{}", provider.host, provider.port);
        let handle = ConnectionHandle::spawn(
            self.rt.handle(),
            addr.clone(),
            ctx.clone(),
            self.settings.send_keepalive,
        );
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
            },
        );
        self.active = Some(id);
        self.remember_session();
    }

    /// Drain network events for every session and apply them to the model.
    fn pump_network(&mut self) {
        let clear_on_disconnect = self.settings.clear_tree_on_disconnect;
        for session in self.sessions.values_mut() {
            for event in session.handle.drain() {
                match event {
                    NetEvent::Connected => {
                        session.status = Status::Connected;
                        // (Re)establish: refetch every expanded node and
                        // re-subscribe visible params on the new connection.
                        for e in session.tree.entries.values_mut() {
                            e.requested = false;
                        }
                        session.subscribed.clear();
                    }
                    NetEvent::Document(root) => session.tree.merge(root),
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
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump_network();
        self.process_pulses(&ctx);

        egui::TopBottomPanel::top("menubar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Options").clicked() {
                    self.show_options = true;
                }
            });
        });

        self.sidebar(ui, &ctx);
        self.add_dialog(&ctx);
        self.options_window(&ctx);
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
                });
                match action {
                    Some(SidebarAction::Open(id)) => self.open_provider(id, ctx),
                    Some(SidebarAction::Disconnect(id)) => self.disconnect(id),
                    Some(SidebarAction::Edit(id)) => self.open_edit_dialog(id),
                    Some(SidebarAction::Remove(id)) => self.remove_provider(id),
                    None => {}
                }
            });
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
                let mut header = egui::CollapsingHeader::new(format!("📁 {}", folder.name))
                    .default_open(true);
                if !filter.is_empty() {
                    header = header.open(Some(true));
                }
                header.show(ui, |ui| {
                    for child in &folder.children {
                        Self::sidebar_node(ui, child, sessions, active, action, filter);
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
                ui.horizontal(|ui| {
                    paint_dot(ui, status_color(status));
                    let resp = ui
                        .selectable_label(selected, &p.name)
                        .on_hover_text(format!("{}:{}", p.host, p.port));
                    if resp.clicked() {
                        *action = Some(SidebarAction::Open(p.id));
                    }
                    resp.context_menu(|ui| {
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
                            *action = Some(SidebarAction::Edit(p.id));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Remove").clicked() {
                            *action = Some(SidebarAction::Remove(p.id));
                            ui.close();
                        }
                    });
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

    /// Remove a provider from the address book (disconnecting first if open).
    fn remove_provider(&mut self, id: Id) {
        self.disconnect(id);
        self.book.remove(id);
        if let Err(e) = self.book.save() {
            self.status_line = format!("could not save address book: {e}");
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
                                self.book.update_provider(id, name.clone(), host, port, None);
                                // Reflect a new display name on an open tab.
                                if let Some(s) = self.sessions.get_mut(&id) {
                                    s.name = name;
                                }
                            }
                            None => {
                                self.book.add_provider(self.add.parent, name, host, port, None);
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
                    .checkbox(&mut self.settings.show_descriptions, "Show descriptions in tree")
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
            });
        self.show_options = open;
        if changed {
            if let Err(e) = self.settings.save() {
                self.status_line = format!("could not save settings: {e}");
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
                    for root in &roots {
                        render_filtered(
                            ui, session, root, allowed, &opts, &mut commands, &mut visible,
                        );
                    }
                } else {
                    for root in &roots {
                        render_entry(ui, session, root, &opts, &mut commands, &mut visible);
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
fn render_entry(
    ui: &mut egui::Ui,
    session: &mut Session,
    path: &[u32],
    opts: &RenderOpts,
    commands: &mut Vec<NetCommand>,
    visible: &mut Vec<Vec<u32>>,
) {
    let Some(entry) = session.tree.get(path).cloned() else {
        return;
    };

    if entry.kind.is_expandable() {
        let is_open = session.open.get(path).copied().unwrap_or(false);
        let id = ui.make_persistent_id(("node", path));
        let header = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            id,
            false,
        );
        let next_open = header.is_open();
        let heading = node_label(&entry, opts);
        let resp = header
            .show_header(ui, |ui| {
                ui.label(heading)
                    .context_menu(|ui| context_copy(ui, &entry.path, &entry.identifier));
            })
            .body(|ui| {
                // Matrix grid / function form, when this node is one.
                if let Some(m) = &entry.matrix {
                    render_matrix(ui, &entry, m, opts, commands);
                } else if let Some(f) = &entry.function {
                    render_function(ui, session, &entry, f, commands);
                }
                let children = sorted_paths(&session.tree, &entry.children, opts.order_by);
                for child in &children {
                    render_entry(ui, session, child, opts, commands, visible);
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
            commands.push(NetCommand::GetDirectory(path.to_vec()));
        }
    } else {
        // A leaf parameter is on screen → eligible for a live subscription.
        visible.push(path.to_vec());
        render_parameter(ui, session, &entry, opts, commands);
    }
}

/// Render a parameter row: label, value, and (if writable) an editor.
fn render_parameter(
    ui: &mut egui::Ui,
    session: &mut Session,
    entry: &crate::model::Entry,
    opts: &RenderOpts,
    commands: &mut Vec<NetCommand>,
) {
    let label = param_label(entry, opts);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        // Read-only vs writable badge.
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
        // Attach the context menu to the (interactive) label, like node headers.
        ui.label(egui::RichText::new(label).strong())
            .on_hover_text(format!("path {}", path_string(&entry.path)))
            .context_menu(|ui| context_copy(ui, &entry.path, &entry.identifier));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if entry.param_type == Some(glow::parameter_type::TRIGGER) {
                if ui.button("Fire").clicked() {
                    commands.push(NetCommand::SetValue(entry.path.clone(), Value::Integer(0)));
                }
            } else if entry.is_writable() {
                editor(ui, session, entry, opts, commands);
            } else if let Some(v) = &entry.value {
                ui.label(format_value(v));
            } else {
                ui.weak("—");
            }
        });
    });
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
            if ui.button("Pulse").on_hover_text("Set true, then false").clicked() {
                commands.push(NetCommand::SetValue(path.clone(), Value::Boolean(true)));
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_millis(opts.pulse_ms);
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
            // Enum parameter → combo of labels; otherwise a numeric field.
            if let Some(labels) = &entry.enumeration {
                let mut sel = *i as usize;
                let current = labels.get(sel).cloned().unwrap_or_else(|| i.to_string());
                egui::ComboBox::from_id_salt(("enum", &entry.path))
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (idx, label) in labels.iter().enumerate() {
                            if ui.selectable_value(&mut sel, idx, label).clicked() {
                                commands
                                    .push(NetCommand::SetValue(path.clone(), Value::Integer(idx as i64)));
                            }
                        }
                    });
            } else {
                text_commit_editor(ui, session, entry, commands, |s| {
                    s.trim().parse::<i64>().ok().map(Value::Integer)
                });
            }
        }
        Some(Value::Real(_)) => {
            text_commit_editor(ui, session, entry, commands, |s| {
                s.trim().parse::<f64>().ok().map(|f| Value::Real(f.into()))
            });
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

/// Paint a small filled status dot inline (drawn, not a font glyph, so it always
/// renders regardless of the available fonts).
fn paint_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, ui.spacing().interact_size.y), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
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
        if matches.iter().any(|m| entry.len() > m.len() && entry.starts_with(m)) {
            allowed.insert(entry.clone());
        }
    }
    allowed
}

/// Render a filtered tree: only `allowed` paths, always expanded. Records
/// visible parameter paths so subscriptions still track what's on screen.
fn render_filtered(
    ui: &mut egui::Ui,
    session: &mut Session,
    path: &[u32],
    allowed: &HashSet<Vec<u32>>,
    opts: &RenderOpts,
    commands: &mut Vec<NetCommand>,
    visible: &mut Vec<Vec<u32>>,
) {
    if !allowed.contains(path) {
        return;
    }
    let Some(entry) = session.tree.get(path).cloned() else {
        return;
    };
    if entry.kind.is_expandable() {
        ui.label(node_label(&entry, opts))
            .context_menu(|ui| context_copy(ui, &entry.path, &entry.identifier));
        ui.indent(("filt", path), |ui| {
            let children = sorted_paths(&session.tree, &entry.children, opts.order_by);
            for child in &children {
                render_filtered(ui, session, child, allowed, opts, commands, visible);
            }
        });
    } else {
        visible.push(path.to_vec());
        render_parameter(ui, session, &entry, opts, commands);
    }
}

/// Render a matrix as a crosspoint grid. With `matrix_targets_on_top`, targets
/// are columns and sources are rows; otherwise the axes are swapped.
fn render_matrix(
    ui: &mut egui::Ui,
    entry: &crate::model::Entry,
    m: &crate::model::MatrixInfo,
    opts: &RenderOpts,
    commands: &mut Vec<NetCommand>,
) {
    let path = entry.path.clone();
    let kind = match m.mtype {
        x if x == glow::matrix_type::ONE_TO_N => "1:N",
        x if x == glow::matrix_type::ONE_TO_ONE => "1:1",
        x if x == glow::matrix_type::N_TO_N => "N:N",
        _ => "?",
    };
    ui.label(format!("Matrix {}×{} ({kind})", m.target_count, m.source_count));

    // The connection state and the click action are axis-independent.
    let mut cell = |ui: &mut egui::Ui, t: u32, s: u32| {
        let on = m.connections.get(&t).is_some_and(|set| set.contains(&s));
        if ui
            .add(egui::SelectableLabel::new(on, "   "))
            .on_hover_text(format!("target {t} ← source {s}"))
            .clicked()
        {
            let operation = if on {
                glow::connection_operation::DISCONNECT
            } else if m.mtype == glow::matrix_type::N_TO_N {
                glow::connection_operation::CONNECT // N:N adds a source
            } else {
                glow::connection_operation::ABSOLUTE // 1:N / 1:1 replace the source
            };
            commands.push(NetCommand::MatrixConnect {
                path: path.clone(),
                target: t,
                sources: vec![s],
                operation,
            });
        }
    };

    let (col_letter, row_letter) = if opts.matrix_targets_on_top {
        ("T", "S")
    } else {
        ("S", "T")
    };
    ui.label(
        egui::RichText::new(format!(
            "columns (top) = {}, rows (left) = {}",
            if opts.matrix_targets_on_top { "targets" } else { "sources" },
            if opts.matrix_targets_on_top { "sources" } else { "targets" },
        ))
        .small()
        .weak(),
    );

    egui::ScrollArea::horizontal()
        .id_salt(("mscroll", &path))
        .show(ui, |ui| {
            // Stronger alternating row shading than the default faint stripe.
            ui.visuals_mut().faint_bg_color = if ui.visuals().dark_mode {
                egui::Color32::from_gray(58)
            } else {
                egui::Color32::from_gray(214)
            };
            egui::Grid::new(("matrix", &path))
                .striped(true)
                .spacing(egui::vec2(2.0, 2.0))
                .show(ui, |ui| {
                    let (cols, rows) = if opts.matrix_targets_on_top {
                        (m.target_count, m.source_count)
                    } else {
                        (m.source_count, m.target_count)
                    };
                    // Header row: corner + column indices.
                    ui.label(egui::RichText::new(format!("{row_letter}\\{col_letter}")).small().weak());
                    for c in 0..cols {
                        ui.label(egui::RichText::new(format!("{col_letter}{c}")).small());
                    }
                    ui.end_row();
                    for r in 0..rows {
                        ui.label(egui::RichText::new(format!("{row_letter}{r}")).small());
                        for c in 0..cols {
                            // Map (row, col) back to (target, source).
                            let (t, s) = if opts.matrix_targets_on_top {
                                (c, r)
                            } else {
                                (r, c)
                            };
                            cell(ui, t, s);
                        }
                        ui.end_row();
                    }
                });
        });
}

/// Render a function's argument form, an Invoke button, and the last result.
fn render_function(
    ui: &mut egui::Ui,
    session: &mut Session,
    entry: &crate::model::Entry,
    f: &crate::model::FunctionInfo,
    commands: &mut Vec<NetCommand>,
) {
    let path = entry.path.clone();
    if f.args.is_empty() {
        ui.weak("no arguments");
    }
    for (i, arg) in f.args.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("{} ({})", arg.name, ptype_name(arg.ptype)));
            let buf = session.func_inputs.entry((path.clone(), i)).or_default();
            ui.add(egui::TextEdit::singleline(buf).desired_width(120.0));
        });
    }
    ui.horizontal(|ui| {
        if ui.button("Invoke").clicked() {
            let args: Vec<Value> = f
                .args
                .iter()
                .enumerate()
                .map(|(i, arg)| {
                    let s = session
                        .func_inputs
                        .get(&(path.clone(), i))
                        .cloned()
                        .unwrap_or_default();
                    parse_value(&s, arg.ptype)
                })
                .collect();
            let id = session.next_invocation_id;
            session.next_invocation_id += 1;
            session.invocations.insert(path.clone(), id);
            commands.push(NetCommand::Invoke {
                path: path.clone(),
                invocation_id: id,
                args,
            });
        }
    });
    if let Some(id) = session.invocations.get(&path) {
        if let Some(outcome) = session.tree.invocation_results.get(id) {
            let names: Vec<String> = if f.result.len() == outcome.values.len() {
                f.result
                    .iter()
                    .zip(&outcome.values)
                    .map(|(slot, v)| format!("{}={}", slot.name, format_value(v)))
                    .collect()
            } else {
                outcome.values.iter().map(format_value).collect()
            };
            let status = if outcome.success { "OK" } else { "FAILED" };
            ui.colored_label(
                if outcome.success {
                    egui::Color32::from_rgb(40, 160, 80)
                } else {
                    egui::Color32::from_rgb(200, 60, 60)
                },
                format!("Result {status}: {}", names.join(", ")),
            );
        }
    }
}

fn ptype_name(ptype: i32) -> &'static str {
    use glow::parameter_type as pt;
    match ptype {
        x if x == pt::INTEGER => "int",
        x if x == pt::REAL => "real",
        x if x == pt::STRING => "string",
        x if x == pt::BOOLEAN => "bool",
        x if x == pt::ENUM => "enum",
        x if x == pt::OCTETS => "octets",
        _ => "?",
    }
}

fn parse_value(s: &str, ptype: i32) -> Value {
    use glow::parameter_type as pt;
    let t = s.trim();
    match ptype {
        x if x == pt::INTEGER || x == pt::ENUM => Value::Integer(t.parse().unwrap_or(0)),
        x if x == pt::REAL => Value::Real(t.parse::<f64>().unwrap_or(0.0).into()),
        x if x == pt::BOOLEAN => {
            Value::Boolean(matches!(t.to_lowercase().as_str(), "true" | "1" | "yes" | "on"))
        }
        _ => Value::String(s.to_string()),
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

fn key_of(entry: Option<&crate::model::Entry>, f: impl Fn(&crate::model::Entry) -> String) -> String {
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
    let buf = session.edits.entry(path.clone()).or_insert_with(|| {
        entry
            .value
            .as_ref()
            .map(format_value)
            .unwrap_or_default()
    });
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
