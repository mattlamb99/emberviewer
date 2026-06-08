//! A UI-friendly tree model built from incoming Glow documents.
//!
//! The model is keyed by integer path (e.g. `0.1.2`). Responses to `getDirectory`
//! are merged in: each element upserts an entry and links it under its parent.
//! Parameters and matrices/functions are leaves (for now); nodes are expandable.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ember_proto::glow::{self, Element, Root, RootElement, Value};

/// What kind of thing a tree entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Node,
    Parameter,
    Matrix,
    Function,
}

impl Kind {
    /// Whether this kind can be expanded to reveal children.
    pub fn is_expandable(self) -> bool {
        matches!(self, Kind::Node | Kind::Matrix | Kind::Function)
    }
}

/// A function argument or result slot.
// Functions/invocations are rendered by the desktop app only for now.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Debug, Clone)]
pub struct TupleItem {
    pub name: String,
    pub ptype: i32,
}

/// Matrix detail, attached to a matrix entry.
#[derive(Debug, Clone, Default)]
pub struct MatrixInfo {
    pub mtype: i32,
    pub target_count: u32,
    pub source_count: u32,
    /// Actual target signal numbers (sparse on real devices); falls back to
    /// `0..target_count` for linear matrices that omit explicit targets.
    pub targets: Vec<u32>,
    /// Actual source signal numbers.
    pub sources: Vec<u32>,
    /// target number -> connected source numbers.
    pub connections: BTreeMap<u32, BTreeSet<u32>>,
    /// Resolved target/source names, keyed by signal number (from label nodes).
    pub target_labels: BTreeMap<u32, String>,
    pub source_labels: BTreeMap<u32, String>,
    /// `labels[].basePath` nodes to fetch for names (RELATIVE-OID arcs).
    pub label_paths: Vec<Vec<u32>>,
    /// `parametersLocation` basePath (RELATIVE-OID arcs), where per-crosspoint /
    /// per-signal / matrix-level parameters (gain, name, …) live, if advertised.
    pub params_location: Option<Vec<u32>>,
    /// `gainParameterNumber`: the sub-number of the gain parameter within a
    /// crosspoint's parameter node, if the matrix advertises one.
    pub gain_param: Option<i32>,
    /// Resolved `parametersLocation/targets` node path; a per-target signal's
    /// parameters live at `param_targets_path + [signal]`.
    pub param_targets_path: Option<Vec<u32>>,
    /// Resolved `parametersLocation/sources` node path.
    pub param_sources_path: Option<Vec<u32>>,
}

/// One enumeration choice for an enum parameter.
#[derive(Debug, Clone)]
pub struct EnumEntry {
    pub value: i64,
    pub label: String,
    /// `~`-prefixed entries are hidden from pickers but keep their index slot.
    pub hidden: bool,
}

/// Function detail, attached to a function entry.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Debug, Clone, Default)]
pub struct FunctionInfo {
    pub args: Vec<TupleItem>,
    pub result: Vec<TupleItem>,
}

/// The outcome of a function invocation.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Debug, Clone)]
pub struct InvocationOutcome {
    pub success: bool,
    pub values: Vec<Value>,
}

/// One node or parameter in the provider tree.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: Vec<u32>,
    pub kind: Kind,
    pub identifier: String,
    pub description: Option<String>,
    pub value: Option<Value>,
    pub param_type: Option<i32>,
    pub access: i32,
    /// Enumeration choices (from `enumeration` or `enumMap`), if an enum.
    pub enum_entries: Vec<EnumEntry>,
    pub minimum: Option<Value>,
    pub maximum: Option<Value>,
    /// Printf-style display format (e.g. "%d dB").
    pub format: Option<String>,
    /// Display divisor for integer values (raw / factor).
    pub factor: Option<i32>,
    /// Stream identifier, if this parameter's value arrives via a stream.
    pub stream_identifier: Option<i32>,
    /// Stream descriptor (format, byte offset) for unpacking a packed stream.
    pub stream_descriptor: Option<(i32, i32)>,
    /// False when the element (or an ancestor) is offline.
    pub is_online: bool,
    /// Ordered child paths discovered so far.
    pub children: Vec<Vec<u32>>,
    /// Whether we have already issued `getDirectory` for this node's children.
    pub requested: bool,
    /// Matrix detail (for `Kind::Matrix`).
    pub matrix: Option<MatrixInfo>,
    /// Function detail (for `Kind::Function`).
    pub function: Option<FunctionInfo>,
}

impl Entry {
    fn new(path: Vec<u32>, kind: Kind) -> Self {
        Entry {
            path,
            kind,
            identifier: String::new(),
            description: None,
            value: None,
            param_type: None,
            access: glow::access::READ,
            enum_entries: Vec::new(),
            minimum: None,
            maximum: None,
            format: None,
            factor: None,
            stream_identifier: None,
            stream_descriptor: None,
            is_online: true,
            children: Vec::new(),
            requested: false,
            matrix: None,
            function: None,
        }
    }

    /// The label for an enum value, if known and visible.
    pub fn enum_label(&self, value: i64) -> Option<&str> {
        self.enum_entries
            .iter()
            .find(|e| e.value == value)
            .map(|e| e.label.as_str())
    }

    /// Whether this parameter is writable.
    pub fn is_writable(&self) -> bool {
        self.access & glow::access::WRITE != 0
    }

    /// Display label (identifier, or the last path arc if unnamed).
    pub fn label(&self) -> String {
        if self.identifier.is_empty() {
            self.path.last().map(|n| n.to_string()).unwrap_or_default()
        } else {
            self.identifier.clone()
        }
    }
}

/// The whole provider tree.
#[derive(Debug, Default)]
pub struct TreeModel {
    pub entries: HashMap<Vec<u32>, Entry>,
    /// Top-level paths, in discovery order.
    pub roots: Vec<Vec<u32>>,
    /// Most recent function results, keyed by invocation id.
    pub invocation_results: HashMap<i32, InvocationOutcome>,
    /// stream identifier -> parameter paths, for routing StreamCollections.
    /// (Multiple parameters can share one identifier for packed streams.)
    pub stream_index: HashMap<i32, Vec<Vec<u32>>>,
}

impl TreeModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, path: &[u32]) -> Option<&Entry> {
        self.entries.get(path)
    }

    /// Merge a decoded `Root` document into the tree.
    pub fn merge(&mut self, root: Root) {
        match root {
            Root::Elements(coll) => {
                for entry in coll.0 {
                    self.ingest_root_element(entry.0);
                }
                self.resolve_matrix_labels();
            }
            Root::InvocationResult(ir) => {
                if let Some(id) = ir.invocation_id {
                    let values = ir
                        .result
                        .map(|t| t.0.into_iter().map(|tv| tv.0).collect())
                        .unwrap_or_default();
                    self.invocation_results.insert(
                        id,
                        InvocationOutcome {
                            success: ir.success.unwrap_or(true),
                            values,
                        },
                    );
                }
            }
            Root::Streams(coll) => self.apply_streams(coll.0),
            Root::StreamsAlt(coll) => self.apply_streams(coll.0),
        }
    }

    /// Route stream entries to their subscribed parameters.
    fn apply_streams(&mut self, entries: Vec<glow::StreamEntryWrap>) {
        for entry in entries {
            let se = entry.0;
            let Some(paths) = self.stream_index.get(&se.stream_identifier).cloned() else {
                continue;
            };
            for path in paths {
                if let Some(e) = self.entries.get_mut(&path) {
                    e.value = Some(stream_value_for(e, &se.stream_value));
                }
            }
        }
    }

    fn ingest_root_element(&mut self, re: RootElement) {
        match re {
            RootElement::QualifiedNode(qn) => {
                self.ingest_node(qn.path.arcs(), qn.contents, qn.children)
            }
            RootElement::QualifiedParameter(qp) => {
                self.ingest_parameter(qp.path.arcs(), qp.contents)
            }
            RootElement::Element(e) => self.ingest_element(&[], e),
            RootElement::QualifiedMatrix(qm) => self.ingest_matrix(
                qm.path.arcs(),
                qm.contents,
                qm.targets,
                qm.sources,
                qm.connections,
            ),
            RootElement::QualifiedFunction(qf) => self.ingest_function(qf.path.arcs(), qf.contents),
        }
    }

    /// Ingest a (possibly nested) non-qualified element under `parent`.
    fn ingest_element(&mut self, parent: &[u32], e: Element) {
        match e {
            Element::Node(n) => {
                let mut path = parent.to_vec();
                path.push(n.number as u32);
                self.ingest_node(path, n.contents, n.children);
            }
            Element::Parameter(p) => {
                let mut path = parent.to_vec();
                path.push(p.number as u32);
                self.ingest_parameter(path, p.contents);
            }
            Element::Matrix(m) => {
                let mut path = parent.to_vec();
                path.push(m.number as u32);
                self.ingest_matrix(path, m.contents, m.targets, m.sources, m.connections);
            }
            Element::Function(f) => {
                let mut path = parent.to_vec();
                path.push(f.number as u32);
                self.ingest_function(path, f.contents);
            }
            Element::Command(_) => {} // requests only
        }
    }

    fn ingest_node(
        &mut self,
        path: Vec<u32>,
        contents: Option<glow::NodeContents>,
        children: Option<glow::ElementCollection>,
    ) {
        self.upsert(path.clone(), Kind::Node);
        if let Some(c) = contents {
            let e = self.entries.get_mut(&path).unwrap();
            if let Some(id) = c.identifier {
                e.identifier = id;
            }
            if c.description.is_some() {
                e.description = c.description;
            }
            if let Some(online) = c.is_online.as_ref().and_then(glow::any_as_bool) {
                // Coming back online → refetch this subtree.
                if online && !e.is_online {
                    e.requested = false;
                }
                e.is_online = online;
            }
        }
        if let Some(coll) = children {
            for entry in coll.0 {
                self.ingest_element(&path, entry.0);
            }
        }
    }

    fn ingest_parameter(&mut self, path: Vec<u32>, contents: Option<glow::ParameterContents>) {
        self.upsert(path.clone(), Kind::Parameter);
        if let Some(c) = contents {
            let e = self.entries.get_mut(&path).unwrap();
            if let Some(id) = c.identifier {
                e.identifier = id;
            }
            if c.description.is_some() {
                e.description = c.description;
            }
            if c.value_.is_some() {
                e.value = c.value_;
            }
            if c.r#type.is_some() {
                e.param_type = c.r#type;
            }
            if let Some(a) = c.access {
                e.access = a;
            }
            if c.minimum.is_some() {
                e.minimum = c.minimum.map(minmax_to_value);
            }
            if c.maximum.is_some() {
                e.maximum = c.maximum.map(minmax_to_value);
            }
            if c.format.is_some() {
                e.format = c.format;
            }
            if let Some(f) = c.factor {
                e.factor = Some(f);
            }
            if let Some(sid) = c.stream_identifier {
                e.stream_identifier = Some(sid);
            }
            if let Some(sd) = &c.stream_descriptor {
                e.stream_descriptor = Some((sd.format, sd.offset));
            }
            if let Some(online) = c.is_online.as_ref().and_then(glow::any_as_bool) {
                e.is_online = online;
            }
            // Enumeration: newline-separated, with `~`-hidden entries; or enumMap.
            if let Some(en) = c.enumeration {
                e.enum_entries = en
                    .split('\n')
                    .enumerate()
                    .map(|(i, s)| {
                        let hidden = s.starts_with('~');
                        EnumEntry {
                            value: i as i64,
                            label: s.trim_start_matches('~').to_string(),
                            hidden,
                        }
                    })
                    .collect();
            }
            if let Some(map) = c.enum_map {
                e.enum_entries = map
                    .0
                    .into_iter()
                    .map(|pair| EnumEntry {
                        value: pair.0.entry_integer as i64,
                        label: pair.0.entry_string,
                        hidden: false,
                    })
                    .collect();
            }
        }
        // Register this parameter for stream routing (after the `e` borrow ends).
        if let Some(sid) = self.entries.get(&path).and_then(|e| e.stream_identifier) {
            let paths = self.stream_index.entry(sid).or_default();
            if !paths.contains(&path) {
                paths.push(path.clone());
            }
        }
    }

    fn ingest_matrix(
        &mut self,
        path: Vec<u32>,
        contents: Option<glow::MatrixContents>,
        targets: Option<glow::TargetCollection>,
        sources: Option<glow::SourceCollection>,
        connections: Option<glow::ConnectionCollection>,
    ) {
        self.upsert(path.clone(), Kind::Matrix);
        // Identifier/description on the entry itself.
        if let Some(c) = &contents {
            let e = self.entries.get_mut(&path).unwrap();
            if let Some(id) = &c.identifier {
                e.identifier = id.clone();
            }
            if c.description.is_some() {
                e.description = c.description.clone();
            }
        }
        let e = self.entries.get_mut(&path).unwrap();
        let info = e.matrix.get_or_insert_with(MatrixInfo::default);
        if let Some(c) = &contents {
            if let Some(t) = c.r#type {
                info.mtype = t;
            }
            if let Some(tc) = c.target_count {
                info.target_count = tc.max(0) as u32;
            }
            if let Some(sc) = c.source_count {
                info.source_count = sc.max(0) as u32;
            }
            // Label sub-node base paths (resolved to source/target names later).
            if let Some(labels) = &c.labels {
                info.label_paths = labels.0.iter().map(|l| l.0.base_path.arcs()).collect();
            }
            // Where the matrix's parameters (gain etc.) live.
            if let Some(glow::ParametersLocation::BasePath(p)) = &c.parameters_location {
                info.params_location = Some(p.arcs());
            }
            if let Some(g) = c.gain_parameter_number {
                info.gain_param = Some(g);
            }
        }
        // Explicit target/source signal numbers (sparse on real devices).
        if let Some(t) = targets {
            info.targets = t.0.iter().map(|e| e.0.number.max(0) as u32).collect();
        }
        if let Some(s) = sources {
            info.sources = s.0.iter().map(|e| e.0.number.max(0) as u32).collect();
        }
        // Fall back to dense 0..count when explicit lists are absent (linear).
        if info.targets.is_empty() {
            info.targets = (0..info.target_count).collect();
        }
        if info.sources.is_empty() {
            info.sources = (0..info.source_count).collect();
        }
        if let Some(conns) = connections {
            for entry in conns.0 {
                let conn = entry.0;
                let target = conn.target.max(0) as u32;
                let srcs: BTreeSet<u32> = conn
                    .sources
                    .as_ref()
                    .map(|r| r.arcs())
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                let op = conn
                    .operation
                    .unwrap_or(glow::connection_operation::ABSOLUTE);
                let set = info.connections.entry(target).or_default();
                match op {
                    glow::connection_operation::CONNECT => set.extend(srcs),
                    glow::connection_operation::DISCONNECT => {
                        for s in &srcs {
                            set.remove(s);
                        }
                    }
                    _ => *set = srcs, // absolute / tally: replace
                }
            }
        }
    }

    fn ingest_function(&mut self, path: Vec<u32>, contents: Option<glow::FunctionContents>) {
        self.upsert(path.clone(), Kind::Function);
        if let Some(c) = contents {
            let e = self.entries.get_mut(&path).unwrap();
            if let Some(id) = c.identifier {
                e.identifier = id;
            }
            if c.description.is_some() {
                e.description = c.description;
            }
            let map_items = |td: glow::TupleDescription| -> Vec<TupleItem> {
                td.0.into_iter()
                    .map(|item| TupleItem {
                        name: item.0.name.unwrap_or_default(),
                        ptype: item.0.r#type,
                    })
                    .collect()
            };
            e.function = Some(FunctionInfo {
                args: c.arguments.map(map_items).unwrap_or_default(),
                result: c.result.map(map_items).unwrap_or_default(),
            });
        }
    }

    /// Resolve matrix source/target names from fetched label sub-trees.
    ///
    /// Convention (seen on Lawo Ruby): a matrix's `labels[].basePath` points to a
    /// node containing `targets`/`sources` sub-nodes, each holding string
    /// parameters whose *number* is the signal id and whose *value* is the name.
    fn resolve_matrix_labels(&mut self) {
        let matrices: Vec<(Vec<u32>, Vec<Vec<u32>>)> = self
            .entries
            .values()
            .filter_map(|e| {
                e.matrix
                    .as_ref()
                    .filter(|m| !m.label_paths.is_empty())
                    .map(|m| (e.path.clone(), m.label_paths.clone()))
            })
            .collect();

        for (mpath, label_paths) in matrices {
            let mut targets = BTreeMap::new();
            let mut sources = BTreeMap::new();
            for base in &label_paths {
                let Some(base_entry) = self.entries.get(base) else {
                    continue;
                };
                for axis_path in base_entry.children.clone() {
                    let Some(axis) = self.entries.get(&axis_path) else {
                        continue;
                    };
                    let id = axis.identifier.to_lowercase();
                    let map = if id.contains("target") {
                        &mut targets
                    } else if id.contains("source") {
                        &mut sources
                    } else {
                        continue;
                    };
                    for pp in axis.children.clone() {
                        let Some(pe) = self.entries.get(&pp) else {
                            continue;
                        };
                        if let Some(Value::String(name)) = &pe.value {
                            if let Some(num) = pp.last() {
                                map.insert(*num, name.clone());
                            }
                        }
                    }
                }
            }
            if targets.is_empty() && sources.is_empty() {
                continue;
            }
            if let Some(m) = self.entries.get_mut(&mpath).and_then(|e| e.matrix.as_mut()) {
                // Names always come from the labels. The grid's signal numbers
                // come from the matrix's own targets/sources lists when it sent
                // them; only fall back to the (sparse) label keys when the matrix
                // gave us nothing but a dense 0..count default.
                let dense_t: Vec<u32> = (0..m.target_count).collect();
                let dense_s: Vec<u32> = (0..m.source_count).collect();
                if !targets.is_empty() {
                    if m.targets == dense_t {
                        m.targets = targets.keys().copied().collect();
                    }
                    m.target_labels = targets;
                }
                if !sources.is_empty() {
                    if m.sources == dense_s {
                        m.sources = sources.keys().copied().collect();
                    }
                    m.source_labels = sources;
                }
            }
        }
        self.resolve_matrix_param_paths();
    }

    /// Resolve a matrix's `parametersLocation/targets` and `/sources` child node
    /// paths (matched by identifier), so per-signal parameters (gain, type, …)
    /// can be addressed as `<base>/<signal>`.
    fn resolve_matrix_param_paths(&mut self) {
        let matrices: Vec<(Vec<u32>, Vec<u32>)> = self
            .entries
            .values()
            .filter_map(|e| {
                e.matrix
                    .as_ref()
                    .and_then(|m| m.params_location.clone())
                    .map(|ploc| (e.path.clone(), ploc))
            })
            .collect();

        for (mpath, ploc) in matrices {
            let Some(base) = self.entries.get(&ploc) else {
                continue;
            };
            let mut tpath = None;
            let mut spath = None;
            for axis_path in base.children.clone() {
                let Some(axis) = self.entries.get(&axis_path) else {
                    continue;
                };
                let id = axis.identifier.to_lowercase();
                if id.contains("target") {
                    tpath = Some(axis_path);
                } else if id.contains("source") {
                    spath = Some(axis_path);
                }
            }
            if let Some(m) = self.entries.get_mut(&mpath).and_then(|e| e.matrix.as_mut()) {
                if tpath.is_some() {
                    m.param_targets_path = tpath;
                }
                if spath.is_some() {
                    m.param_sources_path = spath;
                }
            }
        }
    }

    /// Insert the entry if missing and link it under its parent.
    fn upsert(&mut self, path: Vec<u32>, kind: Kind) {
        if !self.entries.contains_key(&path) {
            self.entries
                .insert(path.clone(), Entry::new(path.clone(), kind));
            self.link_to_parent(&path);
        }
    }

    fn link_to_parent(&mut self, path: &[u32]) {
        if path.len() <= 1 {
            if !self.roots.iter().any(|p| p == path) {
                self.roots.push(path.to_vec());
            }
            return;
        }
        let parent = &path[..path.len() - 1];
        if let Some(p) = self.entries.get_mut(parent) {
            if !p.children.iter().any(|c| c == path) {
                p.children.push(path.to_vec());
            }
        }
        // If the parent isn't known yet, the link is re-established when it
        // arrives (it always carries the same child paths on getDirectory).
    }
}

fn minmax_to_value(m: glow::MinMax) -> Value {
    match m {
        glow::MinMax::Integer(i) => Value::Integer(i),
        glow::MinMax::Real(r) => Value::Real(r),
    }
}

/// Resolve a stream entry's value for a parameter. A direct (non-octet) value is
/// used as-is; a packed octet-string is unpacked per the parameter's
/// StreamDescriptor (format + byte offset).
fn stream_value_for(entry: &Entry, stream_value: &Value) -> Value {
    match (stream_value, entry.stream_descriptor) {
        (Value::Octets(bytes), Some((format, offset))) => {
            unpack_stream(bytes, format, offset.max(0) as usize)
                .unwrap_or_else(|| stream_value.clone())
        }
        _ => stream_value.clone(),
    }
}

/// Unpack one numeric value from packed stream octets at `offset` per
/// `StreamFormat` (X.690 Glow stream formats).
fn unpack_stream(bytes: &[u8], format: i32, offset: usize) -> Option<Value> {
    use glow::Real;
    macro_rules! rd {
        ($n:expr) => {{
            let end = offset + $n;
            if end > bytes.len() {
                return None;
            }
            &bytes[offset..end]
        }};
    }
    let i = |le: bool, n: usize, signed: bool| -> i64 {
        let b = &bytes[offset..offset + n];
        let mut v: u64 = 0;
        if le {
            for (k, &x) in b.iter().enumerate() {
                v |= (x as u64) << (8 * k);
            }
        } else {
            for &x in b {
                v = (v << 8) | x as u64;
            }
        }
        if signed && n < 8 && (v >> (8 * n - 1)) & 1 == 1 {
            v |= !0u64 << (8 * n); // sign-extend
        }
        v as i64
    };
    Some(match format {
        0 => Value::Integer(*rd!(1).first()? as i64), // u8
        8 => Value::Integer(*rd!(1).first()? as i8 as i64), // s8
        2 => Value::Integer(i(false, 2, false)),
        3 => Value::Integer(i(true, 2, false)),
        4 => Value::Integer(i(false, 4, false)),
        5 => Value::Integer(i(true, 4, false)),
        6 => Value::Integer(i(false, 8, false)),
        7 => Value::Integer(i(true, 8, false)),
        10 => Value::Integer(i(false, 2, true)),
        11 => Value::Integer(i(true, 2, true)),
        12 => Value::Integer(i(false, 4, true)),
        13 => Value::Integer(i(true, 4, true)),
        14 => Value::Integer(i(false, 8, true)),
        15 => Value::Integer(i(true, 8, true)),
        20 => Value::Real(Real::from_f64(
            f32::from_be_bytes(rd!(4).try_into().ok()?) as f64
        )),
        21 => Value::Real(Real::from_f64(
            f32::from_le_bytes(rd!(4).try_into().ok()?) as f64
        )),
        22 => Value::Real(Real::from_f64(f64::from_be_bytes(rd!(8).try_into().ok()?))),
        23 => Value::Real(Real::from_f64(f64::from_le_bytes(rd!(8).try_into().ok()?))),
        _ => return None,
    })
}

/// One-line, human-readable rendering of a parameter value.
pub fn format_value(v: &Value) -> String {
    match v {
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format!("{}", r.to_f64()),
        Value::String(s) => s.clone(),
        Value::Boolean(b) => b.to_string(),
        Value::Octets(o) => format!("<{} bytes>", o.len()),
    }
}

/// How long (egui seconds) to wait before re-requesting a label sub-tree node
/// whose directory hasn't come back, and how many times to retry before giving
/// up (so a genuinely empty labels node isn't polled forever - keeping device
/// traffic minimal).
pub const LABEL_FETCH_RETRY_SECS: f64 = 2.0;
pub const LABEL_FETCH_MAX_ATTEMPTS: u8 = 6;

/// Outcome of [`label_fetch_step`]: what the caller should do for one label
/// sub-tree node this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelFetchStep {
    /// Issue a `getDirectory` for this node now.
    pub request: bool,
    /// If `Some`, store this as the node's new (last-request time, attempts).
    pub new_state: Option<(f64, u8)>,
    /// The node is still waiting on its directory (keep the UI repainting).
    pub pending: bool,
}

/// Decide whether to (re)issue a directory request for a label sub-tree node.
///
/// Embedded devices (e.g. Arkona AT300) silently drop `getDirectory` requests
/// issued during the initial discovery burst, so a one-shot fetch can be lost
/// forever and the matrix's labels never resolve. This retries any node that
/// still has no children - throttled to once per [`LABEL_FETCH_RETRY_SECS`] and
/// capped at [`LABEL_FETCH_MAX_ATTEMPTS`] - until its directory populates.
///
/// `state` is the node's prior (last-request time, attempts), `has_children`
/// whether its directory has arrived, `now` the current egui time.
pub fn label_fetch_step(state: Option<(f64, u8)>, has_children: bool, now: f64) -> LabelFetchStep {
    if has_children {
        return LabelFetchStep {
            request: false,
            new_state: None,
            pending: false,
        };
    }
    match state {
        None => LabelFetchStep {
            request: true,
            new_state: Some((now, 1)),
            pending: true,
        },
        Some((last, attempts)) if attempts < LABEL_FETCH_MAX_ATTEMPTS => {
            if now - last >= LABEL_FETCH_RETRY_SECS {
                LabelFetchStep {
                    request: true,
                    new_state: Some((now, attempts + 1)),
                    pending: true,
                }
            } else {
                LabelFetchStep {
                    request: false,
                    new_state: None,
                    pending: true,
                }
            }
        }
        Some(_) => LabelFetchStep {
            request: false,
            new_state: None,
            pending: false, // retry budget exhausted
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_proto::glow::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// An Arkona AT300 matrix [0,2,50,3] declares a label whose basePath is the
    /// sibling "labels" node [0,2,50,0]; that node holds targets/sources sub-nodes
    /// of string params (number = signal, value = name). Once the subtree is
    /// fetched, the matrix's target/source labels must resolve. Uses the device's
    /// real bytes for the matrix + labels/targets/sources nodes.
    #[test]
    fn arkona_matrix_labels_resolve_from_fetched_subtree() {
        let mut tree = TreeModel::new();
        // Real dir of [0,2,50]: "labels" node [0,2,50,0] + "Matrix" [0,2,50,3]
        // (the matrix's labels field carries basePath [0,2,50,0]).
        for r in glow::decode_roots(&unhex("606e6b6ca01d6a1ba0060d0400023200a111310fa0080c066c6162656c73a3030101ffa04b7149a0060d0400023203a13b3139a0080c064d6174726978a203020100a303020101a403020122a503020136aa193017a0157213a0060d0400023200a1090c077072696d617279a5023000")).into_iter().flatten() {
            tree.merge(r);
        }
        // Real dir of [0,2,50,0]: "targets" [..,1] + "sources" [..,2].
        for r in glow::decode_roots(&unhex("60446b42a01f6a1da0070d050002320001a1123110a0090c0774617267657473a3030101ffa01f6a1da0070d050002320002a1123110a0090c07736f7572636573a3030101ff")).into_iter().flatten() {
            tree.merge(r);
        }
        assert_eq!(
            tree.get(&[0, 2, 50, 3])
                .and_then(|e| e.matrix.as_ref())
                .map(|m| m.label_paths.clone()),
            Some(vec![vec![0, 2, 50, 0]]),
            "matrix label basePath must decode from the real bytes"
        );
        tree.merge(params_doc(&[
            (
                &[0, 2, 50, 0, 1, 0],
                "t0",
                Value::String("i_o_module.output[0].sdi".into()),
                3,
            ),
            (
                &[0, 2, 50, 0, 1, 5000],
                "t5000",
                Value::String("re_play.video.delays[0]".into()),
                3,
            ),
            (
                &[0, 2, 50, 0, 2, 0],
                "s0",
                Value::String("i_o_module.input[0].sdi".into()),
                3,
            ),
        ]));

        let m = tree
            .get(&[0, 2, 50, 3])
            .and_then(|e| e.matrix.as_ref())
            .expect("matrix present");
        assert_eq!(
            m.target_labels.get(&0).map(String::as_str),
            Some("i_o_module.output[0].sdi"),
            "target labels did not resolve: {:?}",
            m.target_labels
        );
        assert_eq!(
            m.target_labels.get(&5000).map(String::as_str),
            Some("re_play.video.delays[0]")
        );
        assert_eq!(
            m.source_labels.get(&0).map(String::as_str),
            Some("i_o_module.input[0].sdi")
        );
    }

    /// Build a QualifiedNode root document.
    fn node_doc(path: &[u32], id: &str) -> Root {
        let qn = QualifiedNode {
            path: RelativeOid::from_arcs(path),
            contents: Some(NodeContents {
                identifier: Some(id.into()),
                ..Default::default()
            }),
            children: None,
        };
        Root::from_element(RootElement::QualifiedNode(qn))
    }

    /// Build a document carrying several qualified parameters.
    fn params_doc(params: &[(&[u32], &str, Value, i32)]) -> Root {
        let entries = params
            .iter()
            .map(|(path, id, val, access)| {
                let qp = QualifiedParameter {
                    path: RelativeOid::from_arcs(path),
                    contents: Some(ParameterContents {
                        identifier: Some((*id).into()),
                        value_: Some(val.clone()),
                        access: Some(*access),
                        ..Default::default()
                    }),
                    children: None,
                };
                RootElementEntry(RootElement::QualifiedParameter(qp))
            })
            .collect();
        Root::Elements(RootElementCollection(entries))
    }

    #[test]
    fn merges_nodes_and_links_children() {
        let mut tree = TreeModel::new();
        tree.merge(node_doc(&[0], "Root"));
        tree.merge(node_doc(&[0, 1], "parameters"));

        assert_eq!(tree.roots, vec![vec![0]]);
        assert_eq!(tree.get(&[0]).unwrap().identifier, "Root");
        // [0,1] is linked under [0].
        assert_eq!(tree.get(&[0]).unwrap().children, vec![vec![0, 1]]);
        assert_eq!(tree.get(&[0, 1]).unwrap().kind, Kind::Node);
    }

    #[test]
    fn merges_parameters_with_values_and_access() {
        let mut tree = TreeModel::new();
        tree.merge(node_doc(&[0], "Root"));
        tree.merge(node_doc(&[0, 1], "parameters"));
        tree.merge(params_doc(&[
            (&[0, 1, 0], "gain", Value::Integer(42), access::READ_WRITE),
            (&[0, 1, 1], "mute", Value::Boolean(true), access::READ),
        ]));

        let parent = tree.get(&[0, 1]).unwrap();
        assert_eq!(parent.children, vec![vec![0, 1, 0], vec![0, 1, 1]]);

        let gain = tree.get(&[0, 1, 0]).unwrap();
        assert_eq!(gain.kind, Kind::Parameter);
        assert_eq!(gain.value, Some(Value::Integer(42)));
        assert!(gain.is_writable());

        let mute = tree.get(&[0, 1, 1]).unwrap();
        assert_eq!(mute.value, Some(Value::Boolean(true)));
        assert!(!mute.is_writable());
    }

    #[test]
    fn enum_parsing_and_value_update() {
        let mut tree = TreeModel::new();
        let doc = {
            let qp = QualifiedParameter {
                path: RelativeOid::from_arcs(&[5]),
                contents: Some(ParameterContents {
                    identifier: Some("color".into()),
                    value_: Some(Value::Integer(1)),
                    enumeration: Some("Red\nGreen\nBlue".into()),
                    r#type: Some(parameter_type::ENUM),
                    access: Some(access::READ_WRITE),
                    ..Default::default()
                }),
                children: None,
            };
            Root::from_element(RootElement::QualifiedParameter(qp))
        };
        tree.merge(doc);
        let e = tree.get(&[5]).unwrap();
        let labels: Vec<&str> = e.enum_entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["Red", "Green", "Blue"]);
        assert_eq!(e.enum_label(2), Some("Blue"));

        // A later value-only update (as a provider push) keeps the identifier.
        let update = {
            let qp = QualifiedParameter {
                path: RelativeOid::from_arcs(&[5]),
                contents: Some(ParameterContents {
                    value_: Some(Value::Integer(2)),
                    ..Default::default()
                }),
                children: None,
            };
            Root::from_element(RootElement::QualifiedParameter(qp))
        };
        tree.merge(update);
        let e = tree.get(&[5]).unwrap();
        assert_eq!(e.value, Some(Value::Integer(2)));
        assert_eq!(e.identifier, "color"); // preserved
    }

    #[test]
    fn label_fetch_retries_until_children_then_gives_up() {
        // First sight of an empty node: request immediately.
        let s = label_fetch_step(None, false, 0.0);
        assert!(s.request && s.pending);
        assert_eq!(s.new_state, Some((0.0, 1)));

        // Within the retry window: still pending, but don't re-request.
        let s = label_fetch_step(Some((0.0, 1)), false, 1.0);
        assert!(!s.request && s.pending);
        assert_eq!(s.new_state, None);

        // After the window: re-request (a dropped response self-heals).
        let s = label_fetch_step(Some((0.0, 1)), false, LABEL_FETCH_RETRY_SECS);
        assert!(s.request && s.pending);
        assert_eq!(s.new_state, Some((LABEL_FETCH_RETRY_SECS, 2)));

        // Children arrived: satisfied, never asked again.
        let s = label_fetch_step(Some((10.0, 3)), true, 100.0);
        assert!(!s.request && !s.pending);

        // Budget exhausted with no children: give up (no forever-polling).
        let s = label_fetch_step(Some((90.0, LABEL_FETCH_MAX_ATTEMPTS)), false, 1000.0);
        assert!(!s.request && !s.pending);
    }
}
