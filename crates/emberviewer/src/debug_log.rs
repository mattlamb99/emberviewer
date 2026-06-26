//! Optional, user-toggleable debug logging to a file.
//!
//! The viewer always installs a tracing subscriber with two layers: the usual
//! console output (env-filtered, for developers) and a file layer that stays
//! inert until the GUI's "Enable debug log" option turns it on. When on it
//! captures connection events and - together with ember-net's frame dumping -
//! the full hex of every Ember+ frame sent and received, so a user can attach
//! the file when a device misbehaves in the viewer.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// How many session logs to keep; older ones are pruned when a new one starts.
const KEEP_LOGS: usize = 10;

struct State {
    /// `Some(file)` while logging is enabled; writes are dropped when `None`.
    sink: Arc<Mutex<Option<File>>>,
    /// Path of the active log file, if any.
    current: Mutex<Option<PathBuf>>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// A `MakeWriter` over the shared optional file: writes go to the file when
/// logging is enabled, and are silently dropped when it is off.
#[derive(Clone)]
struct FileSink(Arc<Mutex<Option<File>>>);

impl<'a> MakeWriter<'a> for FileSink {
    type Writer = FileSinkWriter;
    fn make_writer(&'a self) -> Self::Writer {
        FileSinkWriter(self.0.clone())
    }
}

struct FileSinkWriter(Arc<Mutex<Option<File>>>);

impl Write for FileSinkWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut guard) = self.0.lock() {
            if let Some(file) = guard.as_mut() {
                return file.write(buf);
            }
        }
        Ok(buf.len()) // disabled: accept and discard
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok(mut guard) = self.0.lock() {
            if let Some(file) = guard.as_mut() {
                return file.flush();
            }
        }
        Ok(())
    }
}

/// Install the global tracing subscriber. Call once at startup. `console_filter`
/// drives the developer stdout output (from `RUST_LOG`, default `info`).
pub fn init(console_filter: EnvFilter) {
    let sink = Arc::new(Mutex::new(None));
    let console_layer = fmt::layer().with_filter(console_filter);
    // The file layer passes our crates at debug and everything else at warn, so
    // it captures connection events and the info-level frame dumps. The FileSink
    // gates whether anything is actually written (off until enabled).
    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_writer(FileSink(sink.clone()))
        .with_filter(EnvFilter::new(
            "warn,emberviewer=debug,ember_net=debug,ember_proto=debug,ember_web_proto=debug",
        ));
    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .init();
    let _ = STATE.set(State {
        sink,
        current: Mutex::new(None),
    });
}

/// Directory where debug logs are written: `<config>/logs`.
pub fn logs_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("co", "l2", "emberviewer").map(|d| d.config_dir().join("logs"))
}

/// The active log file path, if logging is currently on.
pub fn current_path() -> Option<PathBuf> {
    STATE.get()?.current.lock().ok()?.clone()
}

/// Turn debug logging on or off. Returns the active log file path when enabling.
pub fn set_enabled(on: bool) -> Result<Option<PathBuf>, String> {
    let state = STATE.get().ok_or("debug log not initialised")?;
    if !on {
        ember_net::set_frame_dump(false);
        if let Ok(mut sink) = state.sink.lock() {
            if let Some(mut file) = sink.take() {
                let _ = file.flush();
            }
        }
        if let Ok(mut current) = state.current.lock() {
            *current = None;
        }
        return Ok(None);
    }

    let dir = logs_dir().ok_or("no config directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    prune_old(&dir);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("emberviewer-debug-{secs}.log"));
    let mut file = File::create(&path).map_err(|e| e.to_string())?;
    let _ = writeln!(
        file,
        "emberviewer {} debug log - {} {} - unix {secs}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let _ = file.flush();
    if let Ok(mut sink) = state.sink.lock() {
        *sink = Some(file);
    }
    if let Ok(mut current) = state.current.lock() {
        *current = Some(path.clone());
    }
    // Turn on raw frame dumping now that there's somewhere to put it.
    ember_net::set_frame_dump(true);
    tracing::info!("debug logging enabled");
    Ok(Some(path))
}

/// Keep at most `KEEP_LOGS` session logs: delete the oldest until adding one
/// more stays within the cap.
fn prune_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("emberviewer-debug-") && n.ends_with(".log"))
        })
        .collect();
    if logs.len() < KEEP_LOGS {
        return;
    }
    logs.sort(); // unix-seconds names sort chronologically
    let remove = logs.len() + 1 - KEEP_LOGS;
    for p in logs.into_iter().take(remove) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    // `fmt`, `EnvFilter`, the prelude traits, and the private types all come in
    // via the parent glob; re-importing them would be a (denied) unused import.
    use super::*;

    /// A scoped file layer captures our crates' frame dumps and filters out
    /// unrelated noise - the core of what the debug log relies on.
    #[test]
    fn file_layer_captures_frames_and_filters_noise() {
        let dir = std::env::temp_dir().join(format!("ev-dbglog-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("capture.log");
        let sink = Arc::new(Mutex::new(Some(File::create(&path).unwrap())));
        let layer = fmt::layer()
            .with_ansi(false)
            .with_writer(FileSink(sink.clone()))
            .with_filter(EnvFilter::new("warn,ember_net=debug,emberviewer=debug"));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "ember_net::connection", "TX payload 3 bytes: aabbcc");
            tracing::debug!(target: "some_dependency", "chatty internal detail");
        });
        if let Ok(mut g) = sink.lock() {
            if let Some(f) = g.as_mut() {
                f.flush().unwrap();
            }
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("TX payload 3 bytes: aabbcc"),
            "got: {contents}"
        );
        assert!(
            !contents.contains("chatty internal detail"),
            "got: {contents}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Disabled sink (None) drops everything without erroring.
    #[test]
    fn disabled_sink_swallows_output() {
        let sink: Arc<Mutex<Option<File>>> = Arc::new(Mutex::new(None));
        let mut w = FileSinkWriter(sink);
        assert_eq!(w.write(b"hello").unwrap(), 5);
        w.flush().unwrap();
    }

    #[test]
    fn prune_keeps_within_cap() {
        let dir = std::env::temp_dir().join(format!("ev-dbgprune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..(KEEP_LOGS + 3) {
            File::create(dir.join(format!("emberviewer-debug-{i:04}.log"))).unwrap();
        }
        // An unrelated file must survive pruning.
        File::create(dir.join("notes.txt")).unwrap();
        prune_old(&dir);
        let logs = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("emberviewer-debug-"))
            })
            .count();
        assert_eq!(logs, KEEP_LOGS - 1, "should leave room for one more");
        assert!(dir.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
