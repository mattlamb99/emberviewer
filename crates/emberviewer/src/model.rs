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
    /// target number -> connected source numbers.
    pub connections: BTreeMap<u32, BTreeSet<u32>>,
}

/// Function detail, attached to a function entry.
#[derive(Debug, Clone, Default)]
pub struct FunctionInfo {
    pub args: Vec<TupleItem>,
    pub result: Vec<TupleItem>,
}

/// The outcome of a function invocation.
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
    /// Parsed enumeration labels, if this is an enum parameter.
    pub enumeration: Option<Vec<String>>,
    pub minimum: Option<Value>,
    pub maximum: Option<Value>,
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
            enumeration: None,
            minimum: None,
            maximum: None,
            children: Vec::new(),
            requested: false,
            matrix: None,
            function: None,
        }
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
            Root::Streams(_) => {} // Phase 5
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
            RootElement::QualifiedFunction(qf) => {
                self.ingest_function(qf.path.arcs(), qf.contents)
            }
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
            if let Some(en) = c.enumeration {
                e.enumeration = Some(en.split('\n').map(|s| s.to_string()).collect());
            }
        }
    }

    fn ingest_matrix(
        &mut self,
        path: Vec<u32>,
        contents: Option<glow::MatrixContents>,
        _targets: Option<glow::TargetCollection>,
        _sources: Option<glow::SourceCollection>,
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
                let op = conn.operation.unwrap_or(glow::connection_operation::ABSOLUTE);
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
                td.0
                    .into_iter()
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

    /// Insert the entry if missing and link it under its parent.
    fn upsert(&mut self, path: Vec<u32>, kind: Kind) {
        if !self.entries.contains_key(&path) {
            self.entries.insert(path.clone(), Entry::new(path.clone(), kind));
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

#[cfg(test)]
mod tests {
    use super::*;
    use ember_proto::glow::*;

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
        assert_eq!(
            e.enumeration.as_deref(),
            Some(["Red".to_string(), "Green".into(), "Blue".into()].as_slice())
        );

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
