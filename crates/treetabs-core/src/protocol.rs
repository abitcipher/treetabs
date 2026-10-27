//! Messages between the server and its clients, one JSON object per line.
//!
//! Clients are either a *browser* bridge (the native messaging host, which
//! relays the extension) or a *viewer* (the GUI).

use crate::{NodeId, Op, Tree};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Browser,
    Viewer,
}

/// Client to server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        role: Role,
        name: String,
    },

    // --- from a browser bridge ---
    /// All windows with tabs (`chrome.windows.getAll({populate: true})`),
    /// and the browser profile's id (windows are kept apart per profile).
    BrowserSnapshot {
        windows: Vec<Value>,
        #[serde(default)]
        browser: Option<String>,
        /// Readable browser name ("Firefox", "Brave", …).
        #[serde(default)]
        label: Option<String>,
    },
    /// A `chrome.*` event: listener name such as `tabs.onCreated`, and its arguments.
    BrowserEvent {
        name: String,
        args: Vec<Value>,
    },
    /// Answer to [`ServerMsg::Call`].
    CallResult {
        req: u64,
        #[serde(default)]
        ok: Option<Value>,
        #[serde(default)]
        err: Option<String>,
    },

    // --- from a viewer ---
    /// Apply ops as one undoable step. `req` is echoed in the reply.
    Edit {
        req: u64,
        ops: Vec<Op>,
    },
    /// Open/focus a node in the browser.
    Activate {
        id: NodeId,
    },
    /// Close the browser tabs/windows of open nodes (keeping them as saved).
    CloseInBrowser {
        ids: Vec<NodeId>,
    },
    /// Unload open tabs (`chrome.tabs.discard`): memory is freed, the tabs
    /// stay in their windows and load again when shown.
    Unload {
        ids: Vec<NodeId>,
    },
    /// List the backup copies; answered with [`ServerMsg::Backups`].
    ListBackups,
    /// Write today's daily export now; answered with [`ServerMsg::Backups`].
    ExportNow,
    /// Send one backup's tree; answered with [`ServerMsg::BackupTree`].
    ReadBackup {
        name: String,
    },
    /// Remove trash items older than this many days (0 = keep them).
    SetTrashDays {
        days: u32,
    },
    /// Copy the store to another folder and use it from now on; answered
    /// with [`ServerMsg::StoreMoved`] to every client (then the server
    /// stops), or a [`ServerMsg::Status`] saying why not.
    MoveStore {
        dir: String,
    },
}

/// Server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Sent to viewers on connect: the full tree and its journal position.
    Snapshot { seq: u64, tree: Box<Tree> },
    /// Ops applied since the previous message, in order. `seq` is the
    /// journal position after them.
    Applied { seq: u64, ops: Vec<Op> },
    /// Reply to [`ClientMsg::Edit`]: the inverse ops (for undo), or an error.
    EditResult {
        req: u64,
        #[serde(default)]
        inverse: Vec<Op>,
        #[serde(default)]
        err: Option<String>,
    },
    /// Status line for viewers (browser connected, warnings, …).
    Status { browsers: usize, message: String },

    /// Ask a browser bridge to call `chrome.<api>(...args)` and reply with
    /// [`ClientMsg::CallResult`].
    Call {
        req: u64,
        api: String,
        args: Vec<Value>,
    },
    /// Ask a browser bridge for a fresh [`ClientMsg::BrowserSnapshot`].
    Resync,

    /// Backup copies, newest first, with where they are.
    Backups {
        list: Vec<crate::backups::BackupInfo>,
        dir: String,
        #[serde(default)]
        message: Option<String>,
    },
    /// Names of the browser profiles seen in this store, by profile id.
    Profiles {
        names: std::collections::BTreeMap<String, String>,
    },
    /// Store-wide options (kept in `<store>/settings.json`), and where the
    /// store is.
    StoreOptions {
        trash_days: u32,
        #[serde(default)]
        dir: String,
    },
    /// The store was copied to `dir` and the server stopped: connect again
    /// (a native host exits, and the browser starts it again).
    StoreMoved { dir: String },
    /// The tree of one backup (or why it could not be read).
    BackupTree {
        name: String,
        #[serde(default)]
        tree: Option<Box<Tree>>,
        #[serde(default)]
        err: Option<String>,
    },
}

/// Write one message as a JSON line.
///
/// # Errors
///
/// When the message cannot be serialized or written.
pub fn write_line<T: Serialize>(w: &mut impl Write, msg: &T) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    w.write_all(&buf)?;
    w.flush()
}

/// Read one JSON line. `Ok(None)` at end of stream.
///
/// # Errors
///
/// When reading fails or the line is not a valid message.
pub fn read_line<T: for<'de> Deserialize<'de>>(r: &mut impl BufRead) -> std::io::Result<Option<T>> {
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            break;
        }
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(std::io::Error::other)
}

/// Chrome native messaging framing: 4-byte native-endian length + JSON.
pub mod native {
    use std::io::{Read, Write};

    /// Chrome refuses host-to-browser messages above 1 MB.
    pub const MAX_TO_BROWSER: usize = 1024 * 1024;

    /// Chrome sends at most 64 MiB to a host; a larger length means the
    /// stream is damaged, and must not be allocated.
    pub const MAX_FROM_BROWSER: usize = 64 * 1024 * 1024;

    /// # Errors
    ///
    /// When reading fails, the stream ends inside a message, the length is
    /// over [`MAX_FROM_BROWSER`], or the message is not JSON.
    pub fn read(r: &mut impl Read) -> std::io::Result<Option<serde_json::Value>> {
        let mut len = [0u8; 4];
        match r.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let n = u32::from_ne_bytes(len) as usize;
        if n > MAX_FROM_BROWSER {
            return Err(std::io::Error::other(format!(
                "message too large ({n} bytes, at most {MAX_FROM_BROWSER})"
            )));
        }
        let mut buf = vec![0u8; n];
        r.read_exact(&mut buf)?;
        serde_json::from_slice(&buf)
            .map(Some)
            .map_err(std::io::Error::other)
    }

    /// # Errors
    ///
    /// When the message is over [`MAX_TO_BROWSER`] or cannot be written.
    pub fn write(w: &mut impl Write, v: &serde_json::Value) -> std::io::Result<()> {
        let buf = serde_json::to_vec(v).map_err(std::io::Error::other)?;
        if buf.len() > MAX_TO_BROWSER {
            return Err(std::io::Error::other(format!(
                "message too large for browser ({} bytes)",
                buf.len()
            )));
        }
        let len = u32::try_from(buf.len()).expect("checked against MAX_TO_BROWSER above");
        w.write_all(&len.to_ne_bytes())?;
        w.write_all(&buf)?;
        w.flush()
    }
}

#[cfg(test)]
mod native_tests {
    use super::native;

    #[test]
    fn oversized_length_is_an_error_without_allocating_it() {
        // A 4 GiB length header and no body.
        let mut input: &[u8] = &u32::MAX.to_ne_bytes();
        let err = native::read(&mut input).expect_err("too large");
        assert!(err.to_string().contains("too large"), "{err}");
    }
}
