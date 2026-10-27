//! Chromium IndexedDB (`*.indexeddb.leveldb` folders) reader.
//!
//! Layout used here (Chromium `indexed_db_leveldb_coding`):
//! * every key starts with a prefix: one byte holding the byte widths of
//!   (database id, object store id, index id), then those ids little-endian;
//! * object store records have index id 1, followed by the encoded IDB key;
//! * values are `varint version` + Blink serialized value. Blink may wrap
//!   the payload (`FF 11 02` = Snappy compressed) and adds its own header
//!   before the V8 payload.

pub mod leveldb;
pub mod v8;

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

use leveldb::varint;

#[derive(Debug, thiserror::Error)]
pub enum IdbError {
    #[error(transparent)]
    LevelDb(#[from] leveldb::LevelDbError),
    #[error("record {key}: {msg}")]
    Value { key: String, msg: String },
    #[error("no `currentSessionSnapshot` record found (stores: {0})")]
    NoSnapshot(String),
}

/// An IndexedDB key.
#[derive(Debug, Clone, PartialEq)]
pub enum IdbKey {
    Null,
    String(String),
    Date(f64),
    Number(f64),
    Array(Vec<IdbKey>),
    Binary(Vec<u8>),
}

impl std::fmt::Display for IdbKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdbKey::Null => f.write_str("null"),
            IdbKey::String(s) => write!(f, "{s:?}"),
            IdbKey::Date(d) => write!(f, "Date({d})"),
            IdbKey::Number(n) => write!(f, "{n}"),
            IdbKey::Array(a) => {
                f.write_str("[")?;
                for (i, k) in a.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k}")?;
                }
                f.write_str("]")
            }
            IdbKey::Binary(b) => write!(f, "<{} bytes>", b.len()),
        }
    }
}

/// One object store record.
#[derive(Debug, Clone)]
pub struct IdbRecord {
    pub database_id: u64,
    pub store_id: u64,
    pub store_name: Option<String>,
    pub key: IdbKey,
    /// Decoded value, or the error that prevented decoding.
    pub value: Result<Value, String>,
    pub raw_len: usize,
}

/// All object store records in an IndexedDB folder.
#[derive(Debug, Default)]
pub struct IdbContents {
    pub databases: BTreeMap<u64, String>,
    pub records: Vec<IdbRecord>,
    pub warnings: Vec<String>,
}

struct Prefix {
    db: u64,
    store: u64,
    index: u64,
    len: usize,
}

fn le_int(b: &[u8]) -> u64 {
    b.iter().rev().fold(0, |acc, &x| (acc << 8) | u64::from(x))
}

fn prefix(k: &[u8]) -> Option<Prefix> {
    let first = *k.first()?;
    let db_len = ((first >> 5) & 7) as usize + 1;
    let store_len = ((first >> 2) & 7) as usize + 1;
    let index_len = (first & 3) as usize + 1;
    let total = 1 + db_len + store_len + index_len;
    if k.len() < total {
        return None;
    }
    let mut p = 1;
    let db = le_int(&k[p..p + db_len]);
    p += db_len;
    let store = le_int(&k[p..p + store_len]);
    p += store_len;
    let index = le_int(&k[p..p + index_len]);
    Some(Prefix {
        db,
        store,
        index,
        len: total,
    })
}

fn utf16be(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
}

fn string_with_len(b: &[u8], pos: &mut usize) -> Option<String> {
    // Length and position come from the data: check every step.
    let n = usize::try_from(varint(b, pos).ok()?).ok()?.checked_mul(2)?;
    let end = pos.checked_add(n)?;
    let s = b.get(*pos..end)?;
    *pos = end;
    Some(utf16be(s))
}

fn decode_key(b: &[u8], pos: &mut usize) -> Option<IdbKey> {
    let t = *b.get(*pos)?;
    *pos += 1;
    let f64_at = |pos: &mut usize| -> Option<f64> {
        let s = b.get(*pos..*pos + 8)?;
        *pos += 8;
        Some(f64::from_le_bytes(s.try_into().ok()?))
    };
    Some(match t {
        0 => IdbKey::Null,
        1 => IdbKey::String(string_with_len(b, pos)?),
        2 => IdbKey::Date(f64_at(pos)?),
        3 => IdbKey::Number(f64_at(pos)?),
        4 => {
            let n = varint(b, pos).ok()?;
            let mut v = Vec::new();
            for _ in 0..n {
                v.push(decode_key(b, pos)?);
            }
            IdbKey::Array(v)
        }
        6 => {
            let n = usize::try_from(varint(b, pos).ok()?).ok()?;
            let end = pos.checked_add(n)?;
            let s = b.get(*pos..end)?.to_vec();
            *pos = end;
            IdbKey::Binary(s)
        }
        _ => return None,
    })
}

/// Blink wrapper tags.
const BLINK_TAG: u8 = 0xFF;
const WRAPPER_TAG: u8 = 0x11;
const WRAP_BLOB: u8 = 0x01;
const WRAP_SNAPPY: u8 = 0x02;
const TRAILER_OFFSET_TAG: u8 = 0xFE;

/// Turn a stored IDB value into JSON.
///
/// # Errors
///
/// When the value is truncated, compressed wrongly, kept in an external
/// blob file, or not valid V8 serialization.
pub fn decode_value(v: &[u8]) -> Result<Value, String> {
    let mut pos = 0;
    varint(v, &mut pos)?; // IDB value version
    let mut body: std::borrow::Cow<[u8]> = std::borrow::Cow::Borrowed(&v[pos..]);

    if body.len() >= 3 && body[0] == BLINK_TAG && body[1] == WRAPPER_TAG {
        match body[2] {
            WRAP_SNAPPY => {
                let raw = snap::raw::Decoder::new()
                    .decompress_vec(&body[3..])
                    .map_err(|e| format!("snappy: {e}"))?;
                body = std::borrow::Cow::Owned(raw);
            }
            WRAP_BLOB => {
                return Err("value is stored in an external blob file (not supported yet)".into());
            }
            other => return Err(format!("unknown Blink wrapper type {other}")),
        }
    }

    let b = &body[..];
    let mut p = 0;
    // Blink header: FF <version> [FE <u64 offset> <u32 size>] then V8 FF <version>.
    if b.first() == Some(&BLINK_TAG) {
        p = 1;
        let version = varint(b, &mut p)?;
        if version >= 21 && b.get(p) == Some(&TRAILER_OFFSET_TAG) {
            p += 1 + 8 + 4;
        }
    }
    v8::decode(b.get(p..).ok_or("value too short")?).map_err(|e| e.to_string())
}

/// Read all object store records from an IndexedDB LevelDB folder.
///
/// # Errors
///
/// When the LevelDB files cannot be read (see [`leveldb::read_dir`]).
pub fn read(dir: &Path) -> Result<IdbContents, IdbError> {
    let snap = leveldb::read_dir(dir)?;
    let mut out = IdbContents {
        warnings: snap.warnings.clone(),
        ..Default::default()
    };
    let mut store_names: BTreeMap<(u64, u64), String> = BTreeMap::new();

    for (k, v) in snap.live() {
        let Some(p) = prefix(k) else { continue };
        let rest = &k[p.len..];
        // Database names: prefix(0,0,0) 0xC9 origin name -> db id
        if p.db == 0 && p.store == 0 && p.index == 0 && rest.first() == Some(&201) {
            let mut pos = 1;
            let _origin = string_with_len(rest, &mut pos);
            if let Some(name) = string_with_len(rest, &mut pos) {
                let mut vp = 0;
                if let Ok(id) = varint(v, &mut vp) {
                    out.databases.insert(id, name);
                }
            }
        }
        // Object store names: prefix(db,0,0) 50 <varint store> 0 -> UTF-16BE name
        if p.db != 0 && p.store == 0 && p.index == 0 && rest.first() == Some(&50) {
            let mut pos = 1;
            if let Ok(store) = varint(rest, &mut pos)
                && rest.get(pos) == Some(&0)
                && pos + 1 == rest.len()
            {
                store_names.insert((p.db, store), utf16be(v));
            }
        }
    }

    for (k, v) in snap.live() {
        let Some(p) = prefix(k) else { continue };
        if p.db == 0 || p.store == 0 || p.index != 1 {
            continue;
        }
        let mut pos = p.len;
        let Some(key) = decode_key(k, &mut pos) else {
            out.warnings
                .push(format!("db {} store {}: undecodable key", p.db, p.store));
            continue;
        };
        out.records.push(IdbRecord {
            database_id: p.db,
            store_id: p.store,
            store_name: store_names.get(&(p.db, p.store)).cloned(),
            key,
            value: decode_value(v),
            raw_len: v.len(),
        });
    }
    out.records.sort_by(|a, b| {
        (a.database_id, a.store_id, a.key.to_string()).cmp(&(
            b.database_id,
            b.store_id,
            b.key.to_string(),
        ))
    });
    Ok(out)
}

/// Key of the record that holds the live Tabs Outliner tree.
pub const SNAPSHOT_KEY: &str = "currentSessionSnapshot";

/// Find the Tabs Outliner session array (`data` of `currentSessionSnapshot`).
///
/// # Errors
///
/// When there is no session snapshot record, or it cannot be decoded.
pub fn session_snapshot(contents: &IdbContents) -> Result<&Value, IdbError> {
    let rec = contents
        .records
        .iter()
        .find(|r| r.key == IdbKey::String(SNAPSHOT_KEY.into()))
        .ok_or_else(|| {
            let stores: Vec<String> = contents
                .records
                .iter()
                .map(|r| {
                    r.store_name
                        .clone()
                        .unwrap_or_else(|| r.store_id.to_string())
                })
                .collect();
            IdbError::NoSnapshot(stores.join(", "))
        })?;
    let v = rec.value.as_ref().map_err(|e| IdbError::Value {
        key: rec.key.to_string(),
        msg: e.clone(),
    })?;
    Ok(v.get("data").unwrap_or(v))
}

/// One stored version of the Tabs Outliner snapshot.
#[derive(Debug)]
pub struct SnapshotVersion {
    /// LevelDB sequence number (higher = newer).
    pub seq: u64,
    pub is_current: bool,
    /// The tree, or why this version could not be read.
    pub tree: Result<crate::Tree, String>,
}

/// All versions of `currentSessionSnapshot` still present in the folder,
/// newest first. LevelDB drops old versions on compaction, so how far back
/// this goes depends on when the browser last compacted the database.
///
/// # Errors
///
/// When the LevelDB files cannot be read.
pub fn snapshot_history(dir: &Path) -> Result<Vec<SnapshotVersion>, IdbError> {
    let snap = leveldb::read_dir_with(dir, true)?;
    let is_snapshot_key = |k: &[u8]| {
        let Some(p) = prefix(k) else { return false };
        if p.db == 0 || p.store == 0 || p.index != 1 {
            return false;
        }
        let mut pos = p.len;
        decode_key(k, &mut pos) == Some(IdbKey::String(SNAPSHOT_KEY.into()))
    };
    let mut versions: Vec<(u64, bool, &[u8])> = Vec::new();
    for (k, r) in &snap.records {
        if let Some(v) = &r.value
            && is_snapshot_key(k)
        {
            versions.push((r.seq, true, v));
        }
    }
    for (k, r) in snap.history.iter().flatten() {
        if let Some(v) = &r.value
            && is_snapshot_key(k)
        {
            versions.push((r.seq, false, v));
        }
    }
    versions.sort_by_key(|v| std::cmp::Reverse(v.0));
    versions.dedup_by_key(|v| v.0);
    Ok(versions
        .into_iter()
        .map(|(seq, is_current, raw)| SnapshotVersion {
            seq,
            is_current,
            tree: decode_value(raw).and_then(|v| {
                crate::tabs_outliner::from_value(v.get("data").unwrap_or(&v))
                    .map(|(t, _)| t)
                    .map_err(|e| e.to_string())
            }),
        })
        .collect())
}
