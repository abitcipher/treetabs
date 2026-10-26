//! Minimal read-only LevelDB reader.
//!
//! Chromium's IndexedDB uses a custom comparator (`idb_cmp1`), so general
//! LevelDB crates refuse to open it. We don't need ordered lookups: we scan
//! every table (`.ldb`/`.sst`) and log (`.log`) file and keep the record with
//! the highest sequence number for each key. The live database is never
//! written to; open a copy if the browser is running.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum LevelDbError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{file}: {msg}")]
    Corrupt { file: String, msg: String },
}

/// One record version.
#[derive(Debug, Clone)]
pub struct Record {
    pub seq: u64,
    /// `None` means the key was deleted.
    pub value: Option<Vec<u8>>,
}

/// Latest state of every key in the database.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub records: HashMap<Vec<u8>, Record>,
    /// Files or blocks that could not be read (the rest is still used).
    pub warnings: Vec<String>,
    /// When set, superseded versions are kept here instead of being dropped.
    pub history: Option<Vec<(Vec<u8>, Record)>>,
}

impl Snapshot {
    fn put(&mut self, key: &[u8], seq: u64, value: Option<Vec<u8>>) {
        match self.records.get_mut(key) {
            Some(r) if r.seq == seq => {}
            Some(r) if r.seq > seq => {
                if let Some(h) = &mut self.history {
                    h.push((key.to_vec(), Record { seq, value }));
                }
            }
            Some(r) => {
                let old = std::mem::replace(r, Record { seq, value });
                if let Some(h) = &mut self.history {
                    h.push((key.to_vec(), old));
                }
            }
            None => {
                self.records.insert(key.to_vec(), Record { seq, value });
            }
        }
    }

    /// Live (not deleted) key/value pairs.
    pub fn live(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.records
            .iter()
            .filter_map(|(k, r)| r.value.as_deref().map(|v| (k.as_slice(), v)))
    }
}

/// Read every table and log file in `dir`.
pub fn read_dir(dir: &Path) -> Result<Snapshot, LevelDbError> {
    read_dir_with(dir, false)
}

/// Like [`read_dir`]; with `keep_history` older versions of keys are kept
/// in [`Snapshot::history`] (as far as LevelDB has not compacted them away).
pub fn read_dir_with(dir: &Path, keep_history: bool) -> Result<Snapshot, LevelDbError> {
    let mut snap = Snapshot { history: keep_history.then(Vec::new), ..Default::default() };
    let mut files: Vec<_> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    files.sort();
    let mut found = false;
    for p in files {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
        let res = match p.extension().and_then(|e| e.to_str()) {
            Some("ldb") | Some("sst") => {
                found = true;
                read_table(&fs::read(&p)?, &mut snap)
            }
            Some("log") if name != "LOG" => {
                found = true;
                read_log(&fs::read(&p)?, &mut snap)
            }
            _ => continue,
        };
        if let Err(msg) = res {
            snap.warnings.push(format!("{name}: {msg}"));
        }
    }
    if !found {
        return Err(LevelDbError::Corrupt {
            file: dir.display().to_string(),
            msg: "no .ldb or .log files found".into(),
        });
    }
    Ok(snap)
}

pub(crate) fn varint(b: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut r = 0u64;
    let mut shift = 0;
    loop {
        let c = *b.get(*pos).ok_or("varint past end")?;
        *pos += 1;
        if shift >= 64 {
            return Err("varint too long".into());
        }
        r |= u64::from(c & 0x7f) << shift;
        if c < 0x80 {
            return Ok(r);
        }
        shift += 7;
    }
}

fn slice<'a>(b: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], String> {
    let end = pos.checked_add(n).filter(|&e| e <= b.len()).ok_or("read past end")?;
    let s = &b[*pos..end];
    *pos = end;
    Ok(s)
}

// ---- log files -------------------------------------------------------------

const LOG_BLOCK: usize = 32 * 1024;

fn read_log(data: &[u8], snap: &mut Snapshot) -> Result<(), String> {
    let mut pending: Vec<u8> = Vec::new();
    let mut in_fragment = false;
    for block in data.chunks(LOG_BLOCK) {
        let mut i = 0;
        while i + 7 <= block.len() {
            let len = u16::from_le_bytes([block[i + 4], block[i + 5]]) as usize;
            let kind = block[i + 6];
            if kind == 0 && len == 0 {
                break; // zero padding at the end of a block
            }
            let Some(payload) = block.get(i + 7..i + 7 + len) else {
                snap.warnings.push("log: truncated record".into());
                break;
            };
            i += 7 + len;
            match kind {
                1 => apply_batch(payload, snap),
                2 => {
                    pending.clear();
                    pending.extend_from_slice(payload);
                    in_fragment = true;
                }
                3 if in_fragment => pending.extend_from_slice(payload),
                4 if in_fragment => {
                    pending.extend_from_slice(payload);
                    let full = std::mem::take(&mut pending);
                    apply_batch(&full, snap);
                    in_fragment = false;
                }
                _ => {
                    snap.warnings.push(format!("log: unexpected record type {kind}"));
                    in_fragment = false;
                }
            }
        }
    }
    Ok(())
}

fn apply_batch(b: &[u8], snap: &mut Snapshot) {
    if let Err(e) = try_apply_batch(b, snap) {
        snap.warnings.push(format!("log: bad write batch: {e}"));
    }
}

fn try_apply_batch(b: &[u8], snap: &mut Snapshot) -> Result<(), String> {
    if b.len() < 12 {
        return Err("too short".into());
    }
    let seq = u64::from_le_bytes(b[0..8].try_into().unwrap());
    let count = u32::from_le_bytes(b[8..12].try_into().unwrap());
    let mut pos = 12;
    for k in 0..count as u64 {
        let tag = *b.get(pos).ok_or("batch past end")?;
        pos += 1;
        let klen = varint(b, &mut pos)? as usize;
        let key = slice(b, &mut pos, klen)?;
        match tag {
            1 => {
                let vlen = varint(b, &mut pos)? as usize;
                let v = slice(b, &mut pos, vlen)?;
                snap.put(key, seq + k, Some(v.to_vec()));
            }
            0 => snap.put(key, seq + k, None),
            t => return Err(format!("unknown batch tag {t}")),
        }
    }
    Ok(())
}

// ---- table files -----------------------------------------------------------

const TABLE_MAGIC: u64 = 0xdb47_7524_8b80_fb57;
const FOOTER_LEN: usize = 48;

fn read_table(data: &[u8], snap: &mut Snapshot) -> Result<(), String> {
    if data.len() < FOOTER_LEN {
        return Err("file too small".into());
    }
    let footer = &data[data.len() - FOOTER_LEN..];
    let magic = u64::from_le_bytes(footer[40..48].try_into().unwrap());
    if magic != TABLE_MAGIC {
        return Err("bad table magic".into());
    }
    let mut pos = 0;
    let _meta_off = varint(footer, &mut pos)?;
    let _meta_len = varint(footer, &mut pos)?;
    let idx_off = varint(footer, &mut pos)? as usize;
    let idx_len = varint(footer, &mut pos)? as usize;

    let index = read_block(data, idx_off, idx_len)?;
    for (_, handle) in block_entries(&index)? {
        let mut p = 0;
        let off = varint(&handle, &mut p)? as usize;
        let len = varint(&handle, &mut p)? as usize;
        let block = match read_block(data, off, len) {
            Ok(b) => b,
            Err(e) => {
                snap.warnings.push(format!("table: data block at {off}: {e}"));
                continue;
            }
        };
        let entries = match block_entries(&block) {
            Ok(e) => e,
            Err(e) => {
                snap.warnings.push(format!("table: data block at {off}: {e}"));
                continue;
            }
        };
        for (ikey, value) in entries {
            if ikey.len() < 8 {
                continue;
            }
            let (ukey, trailer) = ikey.split_at(ikey.len() - 8);
            let t = u64::from_le_bytes(trailer.try_into().unwrap());
            let seq = t >> 8;
            match t & 0xff {
                1 => snap.put(ukey, seq, Some(value)),
                0 => snap.put(ukey, seq, None),
                _ => {}
            }
        }
    }
    Ok(())
}

fn read_block(data: &[u8], off: usize, len: usize) -> Result<Vec<u8>, String> {
    let raw = data.get(off..off + len).ok_or("block past end of file")?;
    let compression = *data.get(off + len).ok_or("missing block trailer")?;
    match compression {
        0 => Ok(raw.to_vec()),
        1 => snap::raw::Decoder::new()
            .decompress_vec(raw)
            .map_err(|e| format!("snappy: {e}")),
        c => Err(format!("unsupported block compression {c}")),
    }
}

type KeyValues = Vec<(Vec<u8>, Vec<u8>)>;

fn block_entries(block: &[u8]) -> Result<KeyValues, String> {
    if block.len() < 4 {
        return Err("block too small".into());
    }
    let n_restarts = u32::from_le_bytes(block[block.len() - 4..].try_into().unwrap()) as usize;
    let end = block
        .len()
        .checked_sub(4 + 4 * n_restarts)
        .ok_or("bad restart count")?;
    let mut out = Vec::new();
    let mut key: Vec<u8> = Vec::new();
    let mut pos = 0;
    while pos < end {
        let shared = varint(block, &mut pos)? as usize;
        let non_shared = varint(block, &mut pos)? as usize;
        let vlen = varint(block, &mut pos)? as usize;
        if shared > key.len() {
            return Err("bad shared key prefix".into());
        }
        key.truncate(shared);
        key.extend_from_slice(slice(block, &mut pos, non_shared)?);
        let v = slice(block, &mut pos, vlen)?;
        out.push((key.clone(), v.to_vec()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_newest_wins() {
        let mut snap = Snapshot::default();
        let mut b = Vec::new();
        b.extend_from_slice(&5u64.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&[1, 1, b'k', 1, b'a']);
        b.extend_from_slice(&[1, 1, b'k', 1, b'b']);
        try_apply_batch(&b, &mut snap).unwrap();
        snap.put(b"k", 1, Some(b"old".to_vec()));
        assert_eq!(snap.records[b"k".as_slice()].value.as_deref(), Some(b"b".as_slice()));
    }
}
