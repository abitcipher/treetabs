## Building and installing treetabs

treetabs has two parts that work together:

* **the browser extension**: the tree window and the link to the browser's
  tabs, windows and bookmarks;
* **the native host** (`treetabs-host`): a small program the browser starts
  through *native messaging*. It keeps the tree on disk and does the
  syncing. The extension cannot work without it.

Optionally there is a desktop app (`treetabs-gui`) with the same tree, and a command-line tool (`treetabs`) that also installs the host.

Everything below is for Linux (tested on Ubuntu 24.04 with the snap browsers). The code is Rust and also builds on macOS and Windows, but the install paths for the host differ there and are not automated yet.


### 1. Tools

| Tool | Version used | Install |
|---|---|---|
| Rust | 1.99 (edition 2024; 1.88 or newer is needed) | <https://rustup.rs> |
| wasm target | | `rustup target add wasm32-unknown-unknown` |
| wasm-bindgen CLI | 0.2.129 (must match the `wasm-bindgen` crate in `extension/ui/Cargo.lock`) | `cargo install wasm-bindgen-cli --version 0.2.129` |
| cargo-oxichrome | 0.2.0 | `cargo install cargo-oxichrome` |
| wasm-opt (binaryen) | 116 | `sudo apt install binaryen` |
| zip, git | | `sudo apt install zip git` (for `cargo xtask package`) |
| Node.js | 24 (only for the browser tests) | <https://nodejs.org> |

The desktop app needs the usual libraries for a windowed OpenGL program:

```sh
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev
```

### 2. Build

```sh
git clone <repository> treetabs && cd treetabs
cargo build --release              # target/release/{treetabs,treetabs-host,treetabs-gui}
cargo xtask build-ext --release    # extension/dist/chromium and extension/dist/firefox
```

`cargo xtask build-ext` builds the extension with oxichrome, compiles the tree window (the egui editor) to wasm, and writes the two browser variants:

* `extension/dist/chromium`: Chromium, Google Chrome, Brave, Opera, Edge. Its manifest has a fixed `key`, so the unpacked extension always gets the id `ipglemijkiaehilbokjjppicafjcbcbg`.
* `extension/dist/firefox`: Firefox (add-on id `treetabs@treetabs.dev`).


### 3. Install the native host

`treetabs install-host` registers the host with a browser: it writes the host manifest where that browser looks for it and records which folder holds your tree (the *store*).

```sh
target/release/treetabs install-host --browser <browser> --set-default
```

| `--browser` | Browser | Host manifest goes to |
|---|---|---|
| `chromium` | Chromium (deb, distro package) | `~/.config/chromium/NativeMessagingHosts/` |
| `chromium-snap` | Chromium snap (Ubuntu default) | `~/snap/chromium/common/chromium/NativeMessagingHosts/` |
| `chrome` | Google Chrome | `~/.config/google-chrome/NativeMessagingHosts/` |
| `brave` | Brave (deb) | `~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/` |
| `brave-snap` | Brave snap | `~/snap/brave/current/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/` |
| `opera` | Opera (deb) | `~/.config/google-chrome/NativeMessagingHosts/` (Opera reads Chrome's folder) |
| `opera-snap` | Opera snap | `~/snap/opera/current/.config/google-chrome/NativeMessagingHosts/` |
| `firefox` | Firefox, deb or snap | `~/.mozilla/native-messaging-hosts/` |

Options:

* `--set-default` also writes `~/.config/treetabs/config.json`, so the desktop app and the CLI use the same store.
* `--store DIR` picks the store folder (default `~/.local/share/treetabs`).
* `--extension-id ID` (repeatable) allows an extension id other than the unpacked one, for example the id the Chrome Web Store gave your listing. `--firefox-id ID` does the same for Firefox.

**Snap browsers** (Chromium, Brave, Opera) run confined: they can only start programs and open files inside their own snap folder. For them, `install-host` copies the host to `~/snap/<browser>/common/treetabs/` and puts the store there (`…/treetabs/store`). Each snap browser therefore has its own store. The snap **Firefox** is different: it starts native hosts through the desktop portal, outside its sandbox, so it can share the main store. The first time, Ubuntu asks whether Firefox may start `dev.treetabs.host`: answer *Allow*.

Opera snap: if Opera does not start at all, install its graphics runtime:
```sh
sudo snap install mesa-2404
```

### 4. Load the extension

**Chromium, Chrome, Brave, Opera**

1. Open `chrome://extensions` (Opera: `opera://extensions`).
2. Turn on *Developer mode*.
3. *Load unpacked* and choose `extension/dist/chromium`. For a snap browser, copy that folder somewhere the browser can read first, for example `~/snap/chromium/common/treetabs/extension`.
4. Click the treetabs toolbar button (or press **Alt+T**): the tree window opens.

**Firefox**

1. Open `about:debugging#/runtime/this-firefox`.
2. *Load Temporary Add-on…* and choose `extension/dist/firefox/manifest.json`.A temporary add-on is removed when Firefox closes. For a permanent   install, sign it through addons.mozilla.org
3. Site icons: about:addons > treetabs > Permissions > *Access your data for all websites*. Without it the tree shows generic icons.

### 5. Check that it works

* The tree window's status bar shows *1 browser(s) connected*.
* `target/release/treetabs live` prints the tree from the command line.
* The host writes a log to `<store>/host.log`.

If the status bar says *offline* or nothing appears:

| Symptom | Cause and fix |
|---|---|
| `host.log` does not exist | The browser did not start the host. Check the manifest path in the table above, and that the extension id in it matches (`chrome://extensions` shows the id). For a store install, rerun `install-host` with `--extension-id`. |
| Snap browser, host never starts | The host must be inside `~/snap/<browser>/common/`. Use the `-snap` variant of `--browser`. |
| Firefox snap, host never starts | The portal permission was denied. Reset it: `gdbus call --session --dest org.freedesktop.impl.portal.PermissionStore --object-path /org/freedesktop/impl/portal/PermissionStore --method org.freedesktop.impl.portal.PermissionStore.Delete webextensions dev.treetabs.host`, then reload the add-on and answer *Allow*. |
| Errors in the extension | `chrome://extensions` > treetabs > *Inspect views: service worker* shows the console. |

### 6. Desktop app and command line

```sh
target/release/treetabs-gui                 # same tree, as a desktop window
target/release/treetabs import export.json  # start a store from a Tabs Outliner export
target/release/treetabs live                # print the live tree
target/release/treetabs --help              # all commands
```

The desktop app and the browser host share the store: whichever starts first owns it, the other connects to it.

### 7. Where your data is

Settings > Storage in the tree window shows the folder, and can move the tree to another one.

| What | Where |
|---|---|
| The tree | `<store>/tree.json` + `<store>/journal.jsonl` (every change, written before it counts) |
| Daily copies | `<store>/exports/treetabs-YYYY-MM-DD.json` (the last 30) |
| Snapshots | `<store>/backups/` |
| Store options | `<store>/settings.json` (`bookmarks`, `daily_exports`, `trash_days`) |
| Tree window settings | the browser's local storage (per browser profile) |
| Desktop app settings | `~/.config/treetabs/ui.json` |

### 8. Updating

```sh
git pull
cargo build --release && cargo xtask build-ext --release
target/release/treetabs install-host --browser <browser> --set-default
```

then click *Reload* on treetabs in `chrome://extensions`. For snap browsers, copy the new `extension/dist/chromium` over the old copy first. The store is kept as it is.

### 9. Uninstalling

Remove the extension in the browser, then delete the host manifest (`dev.treetabs.host.json`) from the folder in the table above. The store stays until you delete it yourself.

### 10. Tests

```sh
cargo test --workspace                             # unit and server tests
cd tests/e2e && npm install && npx playwright install chromium
npx playwright test                                # real Chromium with the extension
SNAP_TEST=1 SNAPS=chromium,brave,opera npx playwright test snap.spec.js
FIREFOX_TEST=1 npx playwright test firefox.spec.js # snap Firefox through geckodriver
node docs-screenshots.mjs                          # regenerate docs/images
```

The browser tests use throwaway profiles and stores; they never touch your own tree.
