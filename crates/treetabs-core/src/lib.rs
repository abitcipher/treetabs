//! treetabs core: tree model, Tabs Outliner import/export, portable JSON.

pub mod backups;
pub mod export;
pub mod idb;
pub mod model;
pub mod ops;
pub mod protocol;
pub mod store;
pub mod sync;
pub mod recover;
pub mod tabs_outliner;

pub use model::{EditError, Node, NodeId, NodeKind, Position, TrashItem, Tree};
pub use ops::{NodeFields, Op};
pub use store::Store;

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Import(#[from] tabs_outliner::ImportError),
    #[error(transparent)]
    Idb(#[from] idb::IdbError),
    #[error("unsupported portable format version {0}")]
    Version(u32),
    #[error("unrecognized JSON file (expected a treetabs file or a Tabs Outliner export)")]
    Unknown,
}

/// Where a loaded tree came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Portable,
    TabsOutlinerJson,
    TabsOutlinerIndexedDb,
}

/// A loaded tree plus non-fatal problems found while reading it.
#[derive(Debug)]
pub struct Loaded {
    pub tree: Tree,
    pub format: SourceFormat,
    pub warnings: Vec<String>,
}

/// Load a tree from a portable file, a Tabs Outliner JSON export, or an
/// IndexedDB folder (detected automatically).
pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    if path.is_dir() {
        let contents = idb::read(path)?;
        let (tree, w) = tabs_outliner::from_value(idb::session_snapshot(&contents)?)?;
        let mut warnings = contents.warnings;
        warnings.extend(w.0);
        return Ok(Loaded { tree, format: SourceFormat::TabsOutlinerIndexedDb, warnings });
    }
    let text = std::fs::read_to_string(path)?;
    load_json_str(&text)
}

/// Load from JSON text (portable or Tabs Outliner).
pub fn load_json_str(text: &str) -> Result<Loaded, LoadError> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    if v.get("format").and_then(|f| f.as_str()) == Some(model::FORMAT_NAME) {
        let tree: Tree = serde_json::from_value(v)?;
        if tree.version > model::FORMAT_VERSION {
            return Err(LoadError::Version(tree.version));
        }
        return Ok(Loaded { tree, format: SourceFormat::Portable, warnings: vec![] });
    }
    if v.is_array() {
        let (tree, w) = tabs_outliner::from_value(&v)?;
        return Ok(Loaded { tree, format: SourceFormat::TabsOutlinerJson, warnings: w.0 });
    }
    Err(LoadError::Unknown)
}

/// Serialize a tree in the portable format.
pub fn to_portable_json(tree: &Tree, pretty: bool) -> serde_json::Result<String> {
    if pretty { serde_json::to_string_pretty(tree) } else { serde_json::to_string(tree) }
}

/// Write the portable JSON atomically (temp file + rename), so a crash
/// mid-save never leaves a half-written file.
pub fn save_portable(tree: &Tree, path: &Path, pretty: bool) -> std::io::Result<()> {
    let json = to_portable_json(tree, pretty).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}
