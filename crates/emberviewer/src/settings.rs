//! User settings, persisted as JSON next to the address book.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::address_book::Id;

/// What to do with saved providers when the app launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StartupMode {
    /// Don't connect anything automatically.
    #[default]
    ConnectNone,
    /// Connect to every provider in the address book.
    ConnectAll,
    /// Reconnect whatever was connected when the app last closed.
    ConnectLast,
}

impl StartupMode {
    pub fn label(self) -> &'static str {
        match self {
            StartupMode::ConnectNone => "Connect none",
            StartupMode::ConnectAll => "Connect all",
            StartupMode::ConnectLast => "Connect last session",
        }
    }
}

/// How to order sibling tree elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OrderBy {
    /// By Ember+ number (discovery order).
    #[default]
    Number,
    /// Alphabetically by identifier.
    Identifier,
    /// Alphabetically by description.
    Description,
}

impl OrderBy {
    pub fn label(self) -> &'static str {
        match self {
            OrderBy::Number => "Number",
            OrderBy::Identifier => "Identifier",
            OrderBy::Description => "Description",
        }
    }
}

fn default_pulse_ms() -> u64 {
    300
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub startup_mode: StartupMode,
    /// How long a boolean "Pulse" holds `true` before resetting to `false`.
    #[serde(default = "default_pulse_ms")]
    pub boolean_pulse_ms: u64,
    /// Sort order for sibling tree elements.
    #[serde(default)]
    pub order_by: OrderBy,
    /// Show each element's description alongside its identifier in the tree.
    #[serde(default)]
    pub show_descriptions: bool,
    /// Clear a provider's tree when it disconnects.
    #[serde(default)]
    pub clear_tree_on_disconnect: bool,
    /// Periodically send keep-alive requests to held connections.
    #[serde(default = "default_true")]
    pub send_keepalive: bool,
    /// Matrix orientation: targets across the top (columns) when true; sources
    /// across the top when false.
    #[serde(default = "default_true")]
    pub matrix_targets_on_top: bool,
    /// Optional path to append parameter-change logs to (empty = window only).
    #[serde(default)]
    pub log_file: String,
    /// Provider ids that were connected at last shutdown (for `ConnectLast`).
    #[serde(default)]
    pub last_connected: Vec<Id>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            startup_mode: StartupMode::default(),
            boolean_pulse_ms: default_pulse_ms(),
            order_by: OrderBy::default(),
            show_descriptions: false,
            clear_tree_on_disconnect: false,
            send_keepalive: true,
            matrix_targets_on_top: true,
            log_file: String::new(),
            last_connected: Vec::new(),
        }
    }
}

impl Settings {
    /// `<config_dir>/settings.json`, alongside the address book.
    pub fn store_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("co", "l2", "emberviewer")
            .map(|d| d.config_dir().join("settings.json"))
    }

    /// Load settings, falling back to defaults on any error.
    pub fn load() -> Self {
        let Some(path) = Self::store_path() else {
            return Self::default();
        };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist settings (best effort; returns an error string on failure).
    pub fn save(&self) -> Result<(), String> {
        let path = Self::store_path().ok_or("no config directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }
}
