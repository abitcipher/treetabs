//! The tree editor UI, independent of how it reaches the server.

mod app;
mod bidi;
pub mod i18n;
mod search;
pub mod settings;

pub use app::App;
pub use settings::Settings;
use treetabs_core::Node;
use treetabs_core::protocol::{ClientMsg, ServerMsg};

/// What arrived since the last poll.
#[derive(Default)]
pub struct Poll {
    pub msgs: Vec<ServerMsg>,
    /// The connection dropped; the app will call [`Transport::reconnect`].
    pub lost: bool,
}

/// A connection to the treetabs server.
pub trait Transport {
    fn send(&mut self, msg: &ClientMsg) -> Result<(), String>;
    fn poll(&mut self) -> Poll;
    fn reconnect(&mut self) -> Result<(), String>;
    fn connected(&self) -> bool;
    /// Short text for the status bar.
    fn describe(&self) -> String;
    /// True when this process runs the server.
    fn is_server(&self) -> bool {
        false
    }
    /// Offer `data` as a file named `name` (a download in the browser, a
    /// save dialog on the desktop). Ok: what to tell the user; Ok(None)
    /// when the user cancelled.
    fn save_file(&mut self, _name: &str, _mime: &str, _data: &[u8]) -> Result<Option<String>, String> {
        Err("saving files is not available here".into())
    }
    /// Let the user pick a file to import; it arrives in [`Self::take_file`].
    fn open_file(&mut self) {}
    /// A picked file, once it has been read: name and contents.
    fn take_file(&mut self) -> Option<(String, Vec<u8>)> {
        None
    }
    /// The store moved to `dir` (connect there from now on).
    fn store_moved(&mut self, _dir: &str) {}
    /// Saved UI settings (JSON), kept per device.
    fn load_settings(&self) -> Option<String> {
        None
    }
    fn save_settings(&mut self, _json: &str) {}
    /// Where the site icon of a node can be loaded from (an image loader
    /// must be installed for its scheme). Default: the icon URL the browser
    /// reported, if it is http(s).
    fn favicon_uri(&self, n: &Node) -> Option<String> {
        n.favicon.as_ref().filter(|f| f.starts_with("https://") || f.starts_with("http://")).cloned()
    }
}
