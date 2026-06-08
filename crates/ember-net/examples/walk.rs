//! Headless tree walk against a live Ember+ provider.
//!
//! Connects, then recursively issues `getDirectory` and prints the discovered
//! tree (nodes, parameters, values). Run against the test provider:
//!
//! ```sh
//! cargo run -p ember-net --example walk -- 127.0.0.1:9000
//! ```

use std::time::Duration;

use ember_net::Connection;
use ember_proto::glow::*;

/// Pull every `Root` document the provider sends in response to one request,
/// flattening their elements. Stops on a short idle timeout.
async fn collect_response(conn: &mut Connection) -> Vec<RootElement> {
    let mut elems = Vec::new();
    while let Ok(Some(root)) = conn.next_root_timeout(Duration::from_millis(800)).await {
        if let Root::Elements(coll) = root {
            for RootElementEntry(re) in coll.0 {
                elems.push(re);
            }
        }
    }
    elems
}

/// Summarise a root element: (path, one-line label, whether it's a node to descend into).
fn describe(re: &RootElement) -> (Vec<u32>, String, bool) {
    match re {
        RootElement::QualifiedNode(qn) => {
            let id = qn
                .contents
                .as_ref()
                .and_then(|c| c.identifier.clone())
                .unwrap_or_default();
            (qn.path.arcs(), format!("[node] {id}"), true)
        }
        RootElement::QualifiedParameter(qp) => {
            let c = qp.contents.as_ref();
            let id = c.and_then(|c| c.identifier.clone()).unwrap_or_default();
            let val = c
                .and_then(|c| c.value_.as_ref())
                .map(format_value)
                .unwrap_or_else(|| "-".into());
            let acc = c.and_then(|c| c.access).unwrap_or(access::READ);
            let rw = if acc & access::WRITE != 0 { "rw" } else { "ro" };
            (qp.path.arcs(), format!("{id} = {val} ({rw})"), false)
        }
        RootElement::Element(Element::Node(n)) => {
            let id = n
                .contents
                .as_ref()
                .and_then(|c| c.identifier.clone())
                .unwrap_or_default();
            (vec![n.number as u32], format!("[node] {id}"), true)
        }
        other => (vec![], format!("[other] {other:?}"), false),
    }
}

fn format_value(v: &Value) -> String {
    match v {
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format!("{:.6}", r.to_f64()),
        Value::String(s) => format!("{s:?}"),
        Value::Boolean(b) => b.to_string(),
        Value::Octets(o) => format!("{} bytes", o.len()),
    }
}

/// Depth-first walk starting at `path`.
async fn walk(conn: &mut Connection, path: &[u32], depth: usize) {
    if let Err(e) = conn.get_directory(path).await {
        eprintln!("getDirectory({path:?}) failed: {e}");
        return;
    }
    let children = collect_response(conn).await;
    for re in &children {
        let (child_path, label, is_node) = describe(re);
        let indent = "  ".repeat(depth);
        println!("{indent}{}  {label}", path_str(&child_path));
        if is_node && child_path.len() > path.len() {
            Box::pin(walk(conn, &child_path, depth + 1)).await;
        }
    }
}

fn path_str(p: &[u32]) -> String {
    p.iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

#[tokio::main]
async fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9000".to_string());
    println!("connecting to {addr} ...");
    let mut conn = match Connection::connect(&addr).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };
    println!("connected. walking tree:\n");
    // Root request returns the root node; then descend into it.
    walk(&mut conn, &[], 0).await;
    println!("\ndone.");
}
