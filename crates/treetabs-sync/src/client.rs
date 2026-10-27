//! Connecting to the server, starting one in-process when none is running.

use crate::protocol::{ClientMsg, Role, ServerMsg, read_line, write_line};
use crate::server::{self, ServerHandle, socket_path};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use treetabs_core::store::StoreError;

/// A live connection. Messages from the server arrive on `rx`; when the
/// server goes away `rx` yields `None` (the channel closes).
pub struct Connection {
    writer: Arc<Mutex<UnixStream>>,
    pub rx: Receiver<ServerMsg>,
    /// Set when this process runs the server. Drop it to stop the server.
    pub server: Option<ServerHandle>,
}

impl Connection {
    /// Send one message to the server.
    ///
    /// # Errors
    ///
    /// When the connection is gone.
    ///
    /// # Panics
    ///
    /// If another thread panicked while sending (poisoned lock).
    pub fn send(&self, msg: &ClientMsg) -> std::io::Result<()> {
        let mut w = self.writer.lock().expect("writer lock");
        write_line(&mut *w, msg)
    }

    /// A cloneable sender for other threads.
    pub fn sender(&self) -> Sender {
        Sender(self.writer.clone())
    }

    pub fn is_server(&self) -> bool {
        self.server.is_some()
    }

    /// Split into a sender, the message receiver and the server handle (if
    /// this process runs the server), so they can live on different threads.
    pub fn split(self) -> (Sender, Receiver<ServerMsg>, Option<ServerHandle>) {
        (Sender(self.writer), self.rx, self.server)
    }
}

#[derive(Clone)]
pub struct Sender(Arc<Mutex<UnixStream>>);

impl Sender {
    /// Send one message to the server.
    ///
    /// # Errors
    ///
    /// When the connection is gone.
    ///
    /// # Panics
    ///
    /// If another thread panicked while sending (poisoned lock).
    pub fn send(&self, msg: &ClientMsg) -> std::io::Result<()> {
        let mut w = self.0.lock().expect("writer lock");
        write_line(&mut *w, msg)
    }

    /// Close the connection (the receiving side then ends too).
    pub fn shutdown(&self) {
        let w = self.0.lock().expect("writer lock");
        let _ = w.shutdown(std::net::Shutdown::Both);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("server in {0} did not come up")]
    Timeout(PathBuf),
}

/// Connect to the server for `dir`, or start it in this process.
///
/// `notify` is called (from the reader thread) after each message is queued,
/// for example to wake up a GUI.
///
/// # Errors
///
/// When the server cannot be reached or started (for example the store
/// is damaged).
pub fn connect(
    dir: &Path,
    role: Role,
    name: &str,
    notify: impl Fn() + Send + 'static,
) -> Result<Connection, ConnectError> {
    let sock = socket_path(dir);
    let mut server = None;
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = UnixStream::connect(&sock) {
            stream = Some(s);
            break;
        }
        match server::start(dir) {
            Ok(h) => {
                server = Some(h);
                stream = Some(UnixStream::connect(&sock)?);
                break;
            }
            // Another process holds the store; its socket may not be up yet.
            Err(StoreError::Locked(_)) => thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.into()),
        }
    }
    let stream = stream.ok_or_else(|| ConnectError::Timeout(dir.to_owned()))?;
    let read_half = stream.try_clone()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut r = BufReader::new(read_half);
        loop {
            match read_line::<ServerMsg>(&mut r) {
                Ok(Some(m)) => {
                    if tx.send(m).is_err() {
                        break;
                    }
                    notify();
                }
                Ok(None) => break,
                Err(e) => {
                    log::warn!("bad server message: {e}");
                    break;
                }
            }
        }
        drop(tx);
        notify();
    });
    let conn = Connection {
        writer: Arc::new(Mutex::new(stream)),
        rx,
        server,
    };
    conn.send(&ClientMsg::Hello {
        role,
        name: name.to_owned(),
    })?;
    Ok(conn)
}
