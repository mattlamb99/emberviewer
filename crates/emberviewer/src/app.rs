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
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let book = AddressBook::load().unwrap_or_default();
        App {
            rt,
            book,
            sessions: HashMap::new(),
            active: None,
            add: AddDialog::default(),
            status_line: String::new(),
            provider_filter: String::new(),
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
    }

    fn connect(&mut self, id: Id, ctx: &egui::Context) {
        let Some(provider) = self.book.find_provider(id).cloned() else {
            return;
        };
        let addr = format!("{}:{}", provider.host, provider.port);
        let handle = ConnectionHandle::spawn(self.rt.handle(), addr.clone(), ctx.clone());
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
            },
        );
        self.active = Some(id);
    }

    /// Drain network events for every session and apply them to the model.
    fn pump_network(&mut self) {
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

        self.sidebar(ui, &ctx);
        self.add_dialog(&ctx);
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

        // Collect commands to send, and the set of parameters currently on
        // screen (so we can manage subscriptions), after the render borrow ends.
        let mut commands: Vec<NetCommand> = Vec::new();
        let mut visible: Vec<Vec<u32>> = Vec::new();
        let roots = session.tree.roots.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(allowed) = &filter_set {
                    if allowed.is_empty() {
                        ui.weak("no matches in loaded tree");
                    }
                    for root in &roots {
                        render_filtered(ui, session, root, allowed, &mut commands, &mut visible);
                    }
                } else {
                    for root in &roots {
                        render_entry(ui, session, root, &mut commands, &mut visible);
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
        let resp = header
            .show_header(ui, |ui| {
                ui.label(format!("📁 {}", entry.label()))
                    .context_menu(|ui| context_copy(ui, &entry.path, &entry.identifier));
            })
            .body(|ui| {
                let children = entry.children.clone();
                for child in &children {
                    render_entry(ui, session, child, commands, visible);
                }
                if children.is_empty() {
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
        render_parameter(ui, session, &entry, commands);
    }
}

/// Render a parameter row: label, value, and (if writable) an editor.
fn render_parameter(
    ui: &mut egui::Ui,
    session: &mut Session,
    entry: &crate::model::Entry,
    commands: &mut Vec<NetCommand>,
) {
    ui.horizontal(|ui| {
        ui.add_space(18.0);
        // Attach the context menu to the (interactive) label, like node headers.
        ui.label(egui::RichText::new(entry.label()).strong())
            .on_hover_text(format!("path {}", path_string(&entry.path)))
            .context_menu(|ui| context_copy(ui, &entry.path, &entry.identifier));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if entry.param_type == Some(glow::parameter_type::TRIGGER) {
                if ui.button("Fire").clicked() {
                    commands.push(NetCommand::SetValue(entry.path.clone(), Value::Integer(0)));
                }
            } else if entry.is_writable() {
                editor(ui, session, entry, commands);
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
    commands: &mut Vec<NetCommand>,
) {
    let path = entry.path.clone();
    match &entry.value {
        Some(Value::Boolean(b)) => {
            let mut v = *b;
            if ui.checkbox(&mut v, "").changed() {
                commands.push(NetCommand::SetValue(path, Value::Boolean(v)));
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
        ui.label(format!("📁 {}", entry.label()))
            .context_menu(|ui| context_copy(ui, &entry.path, &entry.identifier));
        ui.indent(("filt", path), |ui| {
            for child in &entry.children {
                render_filtered(ui, session, child, allowed, commands, visible);
            }
        });
    } else {
        visible.push(path.to_vec());
        render_parameter(ui, session, &entry, commands);
    }
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
