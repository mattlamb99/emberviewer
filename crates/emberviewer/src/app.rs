//! The eframe application: address-book sidebar + provider tree browser.

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
}

#[derive(Clone, PartialEq)]
enum Status {
    Connecting,
    Connected,
    Disconnected(String),
}

/// Draft state for the "add provider" dialog.
#[derive(Default)]
struct AddDialog {
    open: bool,
    name: String,
    host: String,
    port: String,
    parent: Id,
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
            },
        );
        self.active = Some(id);
    }

    /// Drain network events for every session and apply them to the model.
    fn pump_network(&mut self) {
        for session in self.sessions.values_mut() {
            for event in session.handle.drain() {
                match event {
                    NetEvent::Connected => session.status = Status::Connected,
                    NetEvent::Document(root) => session.tree.merge(root),
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

        egui::TopBottomPanel::bottom("status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(id) = self.active {
                    if let Some(s) = self.sessions.get(&id) {
                        let (txt, color) = match &s.status {
                            Status::Connecting => ("connecting…".to_string(), egui::Color32::YELLOW),
                            Status::Connected => {
                                (format!("connected · {}", s.addr), egui::Color32::GREEN)
                            }
                            Status::Disconnected(r) => {
                                (format!("disconnected · {r}"), egui::Color32::LIGHT_RED)
                            }
                        };
                        ui.colored_label(color, txt);
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
                ui.separator();

                let root = self.book.root().clone();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut to_connect = None;
                    for child in &root.children {
                        Self::sidebar_node(
                            ui,
                            child,
                            &self.sessions,
                            self.active,
                            &mut to_connect,
                        );
                    }
                    if let Some(id) = to_connect {
                        self.connect(id, ctx);
                    }
                });
            });
    }

    /// Render one address-book node (folder or provider) recursively.
    fn sidebar_node(
        ui: &mut egui::Ui,
        node: &Node,
        sessions: &HashMap<Id, Session>,
        active: Option<Id>,
        to_connect: &mut Option<Id>,
    ) {
        match node {
            Node::Folder(folder) => {
                egui::CollapsingHeader::new(format!("📁 {}", folder.name))
                    .default_open(true)
                    .show(ui, |ui| {
                        for child in &folder.children {
                            Self::sidebar_node(ui, child, sessions, active, to_connect);
                        }
                    });
            }
            Node::Provider(p) => {
                let connected = sessions.contains_key(&p.id);
                let dot = if connected { "🟢" } else { "⚪" };
                let selected = active == Some(p.id);
                let resp = ui.selectable_label(selected, format!("{dot} {}", p.name));
                if resp.clicked() {
                    *to_connect = Some(p.id);
                }
                resp.on_hover_text(format!("{}:{}", p.host, p.port));
            }
        }
    }

    fn add_dialog(&mut self, ctx: &egui::Context) {
        if !self.add.open {
            return;
        }
        let mut open = self.add.open;
        egui::Window::new("Add provider")
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
                    if ui
                        .add_enabled(valid, egui::Button::new("Add"))
                        .clicked()
                    {
                        let port = self.add.port.trim().parse().unwrap_or(DEFAULT_PORT);
                        let name = if self.add.name.trim().is_empty() {
                            self.add.host.clone()
                        } else {
                            self.add.name.clone()
                        };
                        self.book.add_provider(
                            self.add.parent,
                            name,
                            self.add.host.trim().to_string(),
                            port,
                            None,
                        );
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
        });
        ui.separator();

        // Collect commands to send, and the set of parameters currently on
        // screen (so we can manage subscriptions), after the render borrow ends.
        let mut commands: Vec<NetCommand> = Vec::new();
        let mut visible: Vec<Vec<u32>> = Vec::new();
        let roots = session.tree.roots.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for root in &roots {
                    render_entry(ui, session, root, &mut commands, &mut visible);
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
                ui.label(format!("📁 {}", entry.label()));
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
        // Detect a freshly-opened node and lazily request its children once.
        let opened_now = next_open && !is_open;
        session.open.insert(path.to_vec(), next_open);
        let _ = resp;
        if opened_now && !entry.requested {
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
        ui.label(egui::RichText::new(entry.label()).strong());
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
