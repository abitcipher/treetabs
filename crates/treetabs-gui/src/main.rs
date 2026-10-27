//! Desktop app: the tree editor connected over the local socket.

use std::path::PathBuf;
use treetabs_sync::{ClientMsg, Connection, Role};
use treetabs_ui::{Poll, Transport};

/// Connection to the server over its Unix socket (starting it if needed).
struct Socket {
    dir: PathBuf,
    ctx: egui::Context,
    conn: Option<Connection>,
    /// A file picked for import, until the app takes it.
    picked: Option<(String, Vec<u8>)>,
}

impl Transport for Socket {
    fn send(&mut self, msg: &ClientMsg) -> Result<(), String> {
        match &self.conn {
            Some(c) => c.send(msg).map_err(|e| e.to_string()),
            None => Err("not connected".into()),
        }
    }

    fn poll(&mut self) -> Poll {
        let mut p = Poll::default();
        if let Some(c) = &self.conn {
            loop {
                match c.rx.try_recv() {
                    Ok(m) => p.msgs.push(m),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        p.lost = true;
                        break;
                    }
                }
            }
        }
        if p.lost {
            self.conn = None;
        }
        p
    }

    fn reconnect(&mut self) -> Result<(), String> {
        self.conn = None;
        let ctx = self.ctx.clone();
        let c = treetabs_sync::connect(&self.dir, Role::Viewer, "gui", move || {
            ctx.request_repaint()
        })
        .map_err(|e| e.to_string())?;
        self.conn = Some(c);
        Ok(())
    }

    fn connected(&self) -> bool {
        self.conn.is_some()
    }

    fn describe(&self) -> String {
        treetabs_ui::i18n::trf("Store {path}", &[("path", &self.dir.display())])
    }

    fn is_server(&self) -> bool {
        self.conn.as_ref().is_some_and(Connection::is_server)
    }

    fn save_file(
        &mut self,
        name: &str,
        _mime: &str,
        data: &[u8],
    ) -> Result<Option<String>, String> {
        let Some(path) = rfd::FileDialog::new().set_file_name(name).save_file() else {
            return Ok(None);
        };
        std::fs::write(&path, data).map_err(|e| e.to_string())?;
        Ok(Some(path.display().to_string()))
    }

    fn open_file(&mut self) {
        let picked = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .add_filter("*", &["*"])
            .pick_file();
        if let Some(path) = picked {
            match std::fs::read(&path) {
                Ok(data) => {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    self.picked = Some((name, data));
                }
                Err(e) => log::warn!("cannot read {}: {e}", path.display()),
            }
        }
    }

    fn take_file(&mut self) -> Option<(String, Vec<u8>)> {
        self.picked.take()
    }

    fn store_moved(&mut self, dir: &str) {
        self.dir = PathBuf::from(dir);
    }

    fn load_settings(&self) -> Option<String> {
        std::fs::read_to_string(ui_settings_path()).ok()
    }

    fn save_settings(&mut self, json: &str) {
        let p = ui_settings_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, json);
    }
}

/// `~/.config/treetabs/ui.json`, next to the main config.
fn ui_settings_path() -> PathBuf {
    treetabs_sync::config::user_config_path().with_file_name("ui.json")
}

/// Usage: `treetabs-gui [--store DIR] [FILE-TO-IMPORT]`
fn main() -> eframe::Result {
    env_logger::init();
    let mut args = std::env::args_os().skip(1);
    let mut store = None;
    let mut import = None;
    while let Some(a) = args.next() {
        if a == "--store" {
            store = args.next().map(PathBuf::from);
        } else {
            import = Some(PathBuf::from(a));
        }
    }
    let dir = store.unwrap_or_else(treetabs_sync::config::store_dir);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_app_id("treetabs")
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!(
                    "../../../extension/static/icons/icon256.png"
                ))
                .unwrap_or_default(),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "treetabs",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            let mut t = Socket {
                dir,
                ctx: cc.egui_ctx.clone(),
                conn: None,
                picked: None,
            };
            let _ = t.reconnect();
            Ok(Box::new(treetabs_ui::App::new(Box::new(t), import)))
        }),
    )
}
