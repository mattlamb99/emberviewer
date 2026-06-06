//! Address book data model and persistence.
//!
//! Stores Ember+ provider connection entries organised into nested folders,
//! similar to EmberPlusView. The tree is serialised to JSON in the
//! OS-appropriate per-user config directory.
//!
//! This module is intentionally independent of the Ember+ protocol code.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Default TCP port used by Ember+ providers (S101 over TCP).
pub const DEFAULT_PORT: u16 = 9000;

/// Stable unique identifier for folders and provider entries.
///
/// Allocated by an incrementing counter held by [`AddressBook`].
pub type Id = u64;

/// Errors that can occur while loading or saving the address book.
#[derive(Debug, thiserror::Error)]
pub enum AddressBookError {
    /// The per-user config directory could not be resolved.
    #[error("could not determine config directory")]
    NoConfigDir,

    /// An I/O error occurred while reading or writing the store file.
    #[error("io error at {path}: {source}")]
    Io {
        /// Path that was being accessed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// (De)serialisation of the JSON store failed.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// A single Ember+ provider connection entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// Stable unique id.
    pub id: Id,
    /// Human-readable display name.
    pub name: String,
    /// Host name or IP address.
    pub host: String,
    /// TCP port. Defaults to [`DEFAULT_PORT`].
    pub port: u16,
    /// Optional free-form description / notes.
    pub description: Option<String>,
}

/// A folder grouping child nodes (sub-folders and providers).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Stable unique id.
    pub id: Id,
    /// Folder name.
    pub name: String,
    /// Ordered list of children.
    pub children: Vec<Node>,
}

/// A node in the address book tree: either a folder or a provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    /// A sub-folder.
    Folder(Folder),
    /// A provider entry.
    Provider(Provider),
}

impl Node {
    /// Returns the id of this node, regardless of its kind.
    pub fn id(&self) -> Id {
        match self {
            Node::Folder(f) => f.id,
            Node::Provider(p) => p.id,
        }
    }

    /// Returns the name of this node, regardless of its kind.
    pub fn name(&self) -> &str {
        match self {
            Node::Folder(f) => &f.name,
            Node::Provider(p) => &p.name,
        }
    }

    /// Returns `true` if this node is a folder.
    pub fn is_folder(&self) -> bool {
        matches!(self, Node::Folder(_))
    }
}

/// The complete address book: a root folder plus an id allocator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressBook {
    /// Implicit root folder. Its own id is [`AddressBook::ROOT_ID`].
    root: Folder,
    /// Next id to hand out.
    next_id: Id,
}

impl Default for AddressBook {
    fn default() -> Self {
        Self {
            root: Folder {
                id: Self::ROOT_ID,
                name: "Address Book".to_string(),
                children: Vec::new(),
            },
            next_id: 1,
        }
    }
}

impl AddressBook {
    /// Id of the implicit root folder. Passing this to `add_*` adds to the
    /// top level of the tree.
    pub const ROOT_ID: Id = 0;

    /// Creates an empty address book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a fresh unique id.
    fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The implicit root folder (read-only access).
    pub fn root(&self) -> &Folder {
        &self.root
    }

    /// Adds a new folder under `parent` (use [`AddressBook::ROOT_ID`] for the
    /// top level). Returns the new folder's id, or `None` if `parent` does not
    /// exist or is not a folder.
    pub fn add_folder(&mut self, parent: Id, name: impl Into<String>) -> Option<Id> {
        let id = self.alloc_id();
        let folder = Folder {
            id,
            name: name.into(),
            children: Vec::new(),
        };
        let parent = self.folder_mut(parent)?;
        parent.children.push(Node::Folder(folder));
        Some(id)
    }

    /// Adds a new provider under `parent` (use [`AddressBook::ROOT_ID`] for the
    /// top level). Returns the new provider's id, or `None` if `parent` does not
    /// exist or is not a folder.
    pub fn add_provider(
        &mut self,
        parent: Id,
        name: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        description: Option<String>,
    ) -> Option<Id> {
        let id = self.alloc_id();
        let provider = Provider {
            id,
            name: name.into(),
            host: host.into(),
            port,
            description,
        };
        let parent = self.folder_mut(parent)?;
        parent.children.push(Node::Provider(provider));
        Some(id)
    }

    /// Renames the folder or provider with the given id. Returns `true` if a
    /// node was found and renamed.
    pub fn rename(&mut self, id: Id, name: impl Into<String>) -> bool {
        if id == Self::ROOT_ID {
            self.root.name = name.into();
            return true;
        }
        match self.node_mut(id) {
            Some(Node::Folder(f)) => {
                f.name = name.into();
                true
            }
            Some(Node::Provider(p)) => {
                p.name = name.into();
                true
            }
            None => false,
        }
    }

    /// Updates a provider's editable fields. Returns `true` if `id` is a
    /// provider, `false` otherwise (leaving the tree unchanged).
    pub fn update_provider(
        &mut self,
        id: Id,
        name: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        description: Option<String>,
    ) -> bool {
        match self.node_mut(id) {
            Some(Node::Provider(p)) => {
                p.name = name.into();
                p.host = host.into();
                p.port = port;
                p.description = description;
                true
            }
            _ => false,
        }
    }

    /// Removes the node with `id` from the tree and returns it. The root cannot
    /// be removed. Returns `None` if not found.
    pub fn remove(&mut self, id: Id) -> Option<Node> {
        if id == Self::ROOT_ID {
            return None;
        }
        Self::remove_from(&mut self.root, id)
    }

    /// Moves the node with `id` to become a child of `new_parent`.
    ///
    /// Returns `true` on success. Fails (returning `false`, leaving the tree
    /// unchanged) if: the node does not exist, the new parent does not exist or
    /// is not a folder, or the move would place a folder inside itself or one of
    /// its own descendants.
    pub fn move_node(&mut self, id: Id, new_parent: Id) -> bool {
        if id == Self::ROOT_ID {
            return false;
        }
        // Disallow moving a folder into itself or a descendant.
        if id == new_parent || self.is_descendant(new_parent, id) {
            return false;
        }
        // Validate the destination is an existing folder before detaching.
        if self.folder_mut(new_parent).is_none() {
            return false;
        }
        let node = match Self::remove_from(&mut self.root, id) {
            Some(n) => n,
            None => return false,
        };
        // Destination existence already checked; re-borrow to insert.
        let parent = self
            .folder_mut(new_parent)
            .expect("destination validated above");
        parent.children.push(node);
        true
    }

    /// Finds a node by id (immutable).
    pub fn find(&self, id: Id) -> Option<&Node> {
        if id == Self::ROOT_ID {
            return None; // root is not a Node
        }
        Self::find_in(&self.root, id)
    }

    /// Returns a borrowed reference to the provider with `id`, if present.
    pub fn find_provider(&self, id: Id) -> Option<&Provider> {
        match self.find(id) {
            Some(Node::Provider(p)) => Some(p),
            _ => None,
        }
    }

    /// Iterates over every node in the tree in depth-first pre-order. The
    /// `depth` is the nesting level (root's direct children are depth 0).
    pub fn iter(&self) -> impl Iterator<Item = (usize, &Node)> {
        let mut out: Vec<(usize, &Node)> = Vec::new();
        Self::collect(&self.root, 0, &mut out);
        out.into_iter()
    }

    // ---- internal helpers -------------------------------------------------

    /// Mutable access to the folder with `id` (including the root).
    fn folder_mut(&mut self, id: Id) -> Option<&mut Folder> {
        if id == Self::ROOT_ID {
            return Some(&mut self.root);
        }
        Self::folder_mut_in(&mut self.root, id)
    }

    fn folder_mut_in(folder: &mut Folder, id: Id) -> Option<&mut Folder> {
        for child in &mut folder.children {
            if let Node::Folder(f) = child {
                if f.id == id {
                    return Some(f);
                }
                if let Some(found) = Self::folder_mut_in(f, id) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn node_mut(&mut self, id: Id) -> Option<&mut Node> {
        Self::node_mut_in(&mut self.root, id)
    }

    fn node_mut_in(folder: &mut Folder, id: Id) -> Option<&mut Node> {
        for child in &mut folder.children {
            if child.id() == id {
                return Some(child);
            }
            if let Node::Folder(f) = child {
                if let Some(found) = Self::node_mut_in(f, id) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn find_in(folder: &Folder, id: Id) -> Option<&Node> {
        for child in &folder.children {
            if child.id() == id {
                return Some(child);
            }
            if let Node::Folder(f) = child {
                if let Some(found) = Self::find_in(f, id) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn remove_from(folder: &mut Folder, id: Id) -> Option<Node> {
        if let Some(pos) = folder.children.iter().position(|c| c.id() == id) {
            return Some(folder.children.remove(pos));
        }
        for child in &mut folder.children {
            if let Node::Folder(f) = child {
                if let Some(removed) = Self::remove_from(f, id) {
                    return Some(removed);
                }
            }
        }
        None
    }

    /// Returns `true` if `maybe_descendant` is `ancestor` or lies within the
    /// subtree rooted at `ancestor`.
    fn is_descendant(&self, maybe_descendant: Id, ancestor: Id) -> bool {
        if ancestor == Self::ROOT_ID {
            // Everything is a descendant of the root.
            return maybe_descendant != Self::ROOT_ID;
        }
        if maybe_descendant == ancestor {
            return true;
        }
        match self.find(ancestor) {
            Some(Node::Folder(f)) => Self::subtree_contains(f, maybe_descendant),
            _ => false,
        }
    }

    fn subtree_contains(folder: &Folder, id: Id) -> bool {
        for child in &folder.children {
            if child.id() == id {
                return true;
            }
            if let Node::Folder(f) = child {
                if Self::subtree_contains(f, id) {
                    return true;
                }
            }
        }
        false
    }

    fn collect<'a>(folder: &'a Folder, depth: usize, out: &mut Vec<(usize, &'a Node)>) {
        for child in &folder.children {
            out.push((depth, child));
            if let Node::Folder(f) = child {
                Self::collect(f, depth + 1, out);
            }
        }
    }

    // ---- persistence ------------------------------------------------------

    /// Resolves the path to the JSON store file in the per-user config dir.
    ///
    /// On Linux this is typically
    /// `~/.config/emberviewer/address_book.json`, on macOS
    /// `~/Library/Application Support/co.l2.emberviewer/address_book.json`,
    /// and on Windows
    /// `%APPDATA%\l2\emberviewer\config\address_book.json`.
    pub fn store_path() -> Result<PathBuf, AddressBookError> {
        let dirs = ProjectDirs::from("co", "l2", "emberviewer")
            .ok_or(AddressBookError::NoConfigDir)?;
        Ok(dirs.config_dir().join("address_book.json"))
    }

    /// Loads the address book from the default store path, returning a fresh
    /// empty book if the file does not exist.
    pub fn load() -> Result<Self, AddressBookError> {
        Self::load_from(Self::store_path()?)
    }

    /// Loads the address book from an explicit path, returning a fresh empty
    /// book if the file does not exist.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, AddressBookError> {
        let path = path.as_ref();
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(AddressBookError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Saves the address book to the default store path, creating parent
    /// directories as needed.
    pub fn save(&self) -> Result<(), AddressBookError> {
        self.save_to(Self::store_path()?)
    }

    /// Saves the address book to an explicit path, creating parent directories
    /// as needed.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), AddressBookError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AddressBookError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, json).map_err(|source| AddressBookError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a nested tree:
    ///
    /// ```text
    /// root
    /// ├── Studio A (folder)
    /// │   ├── Mixer (provider)
    /// │   └── Routers (folder)
    /// │       └── Router 1 (provider)
    /// └── Lab (provider)
    /// ```
    fn sample() -> (AddressBook, Id, Id, Id, Id, Id) {
        let mut ab = AddressBook::new();
        let studio = ab.add_folder(AddressBook::ROOT_ID, "Studio A").unwrap();
        let mixer = ab
            .add_provider(studio, "Mixer", "10.0.0.1", DEFAULT_PORT, None)
            .unwrap();
        let routers = ab.add_folder(studio, "Routers").unwrap();
        let router1 = ab
            .add_provider(
                routers,
                "Router 1",
                "10.0.0.2",
                9001,
                Some("primary".into()),
            )
            .unwrap();
        let lab = ab
            .add_provider(AddressBook::ROOT_ID, "Lab", "127.0.0.1", DEFAULT_PORT, None)
            .unwrap();
        (ab, studio, mixer, routers, router1, lab)
    }

    #[test]
    fn builds_nested_tree() {
        let (ab, studio, mixer, routers, router1, lab) = sample();

        // Unique ids.
        let ids = [studio, mixer, routers, router1, lab];
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "ids must be unique");

        // Root has two top-level children.
        assert_eq!(ab.root().children.len(), 2);

        // iter() yields all five nodes with correct depths.
        let all: Vec<_> = ab.iter().collect();
        assert_eq!(all.len(), 5);
        let mixer_entry = all.iter().find(|(_, n)| n.id() == mixer).unwrap();
        assert_eq!(mixer_entry.0, 1); // inside Studio A
        let router_entry = all.iter().find(|(_, n)| n.id() == router1).unwrap();
        assert_eq!(router_entry.0, 2); // inside Studio A / Routers

        // Provider fields preserved.
        let p = ab.find_provider(router1).unwrap();
        assert_eq!(p.host, "10.0.0.2");
        assert_eq!(p.port, 9001);
        assert_eq!(p.description.as_deref(), Some("primary"));
    }

    #[test]
    fn add_to_missing_parent_fails() {
        let mut ab = AddressBook::new();
        assert!(ab.add_folder(999, "nope").is_none());
        assert!(ab
            .add_provider(999, "nope", "h", DEFAULT_PORT, None)
            .is_none());

        // Cannot add into a provider (not a folder).
        let p = ab
            .add_provider(AddressBook::ROOT_ID, "P", "h", DEFAULT_PORT, None)
            .unwrap();
        assert!(ab.add_folder(p, "child").is_none());
    }

    #[test]
    fn rename_by_id() {
        let (mut ab, studio, mixer, ..) = sample();
        assert!(ab.rename(studio, "Studio Alpha"));
        assert!(ab.rename(mixer, "Main Mixer"));
        assert!(!ab.rename(424242, "ghost"));

        assert_eq!(ab.find(studio).unwrap().name(), "Studio Alpha");
        assert_eq!(ab.find(mixer).unwrap().name(), "Main Mixer");

        // Root rename works too.
        assert!(ab.rename(AddressBook::ROOT_ID, "My Book"));
        assert_eq!(ab.root().name, "My Book");
    }

    #[test]
    fn remove_by_id() {
        let (mut ab, _studio, mixer, routers, router1, _lab) = sample();

        // Remove a leaf provider.
        let removed = ab.remove(mixer).unwrap();
        assert_eq!(removed.id(), mixer);
        assert!(ab.find(mixer).is_none());
        assert_eq!(ab.iter().count(), 4);

        // Remove a folder takes its subtree (router1) with it.
        let removed = ab.remove(routers).unwrap();
        assert!(removed.is_folder());
        assert!(ab.find(routers).is_none());
        assert!(ab.find(router1).is_none());
        assert_eq!(ab.iter().count(), 2);

        // Removing again / unknown / root all yield None.
        assert!(ab.remove(routers).is_none());
        assert!(ab.remove(999).is_none());
        assert!(ab.remove(AddressBook::ROOT_ID).is_none());
    }

    #[test]
    fn move_node_between_folders() {
        let (mut ab, studio, mixer, routers, _router1, _lab) = sample();

        // Move Mixer from Studio A into Routers.
        assert!(ab.move_node(mixer, routers));
        // Studio A now has only the Routers folder directly.
        let studio_folder = match ab.find(studio).unwrap() {
            Node::Folder(f) => f,
            _ => panic!(),
        };
        assert_eq!(studio_folder.children.len(), 1);
        // Mixer is now under Routers (depth 2).
        let mixer_entry = ab.iter().find(|(_, n)| n.id() == mixer).unwrap();
        assert_eq!(mixer_entry.0, 2);

        // Move Routers to the root.
        assert!(ab.move_node(routers, AddressBook::ROOT_ID));
        assert_eq!(ab.root().children.len(), 3);
    }

    #[test]
    fn move_rejects_cycles_and_bad_targets() {
        let (mut ab, studio, _mixer, routers, _router1, _lab) = sample();

        // Cannot move a folder into itself.
        assert!(!ab.move_node(studio, studio));
        // Cannot move a folder into its own descendant.
        assert!(!ab.move_node(studio, routers));
        // Cannot move into a non-existent parent.
        assert!(!ab.move_node(routers, 999));
        // Cannot move the root.
        assert!(!ab.move_node(AddressBook::ROOT_ID, routers));
        // Unknown node cannot be moved.
        assert!(!ab.move_node(999, AddressBook::ROOT_ID));

        // Tree unchanged by the failed moves.
        assert_eq!(ab.iter().count(), 5);
    }

    #[test]
    fn serde_json_round_trip() {
        let (ab, ..) = sample();
        let json = serde_json::to_string_pretty(&ab).unwrap();
        let back: AddressBook = serde_json::from_str(&json).unwrap();
        assert_eq!(ab, back);
    }

    #[test]
    fn save_and_load_from_file() {
        let dir = std::env::temp_dir().join(format!("emberviewer_test_{}", std::process::id()));
        let path = dir.join("address_book.json");
        let _ = std::fs::remove_dir_all(&dir);

        let (ab, ..) = sample();
        ab.save_to(&path).unwrap();
        let loaded = AddressBook::load_from(&path).unwrap();
        assert_eq!(ab, loaded);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_missing_returns_default() {
        let path = std::env::temp_dir().join("emberviewer_definitely_missing_xyz.json");
        let _ = std::fs::remove_file(&path);
        let ab = AddressBook::load_from(&path).unwrap();
        assert_eq!(ab, AddressBook::default());
    }

    #[test]
    fn store_path_uses_project_dirs() {
        // May be None in headless CI without HOME; only assert the suffix when present.
        if let Ok(p) = AddressBook::store_path() {
            assert!(p.ends_with("address_book.json"));
        }
    }
}
