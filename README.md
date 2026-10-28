<p align="center"><img src="extension/static/icons/treetabs.svg" width="128" alt="treetabs logo"></p>

## TreeTabs

It keeps a tree of your browser windows and tabs that doesn't lose data, 
mirrors that tree into a bookmark folder, and stays fast with tens of thousands of nodes.

![treetabs](docs/images/overview.png)

**Documentation**

* [User guide](docs/USER-GUIDE.md): installing and using treetabs with and without the companion app, and switching between them.
* [Guide](docs/GUIDE.md): what treetabs does, with screenshots.
* [Building and installing](docs/INSTALL.md): tools, build, native host, loading the extension in each browser, troubleshooting.

Version 0.4.6. Works with Chromium, Google Chrome, Brave, Opera and Firefox (including the Ubuntu snaps), in 15 interface languages.

---

### How it fits together

```text
 browser ── extension (Rust/wasm, oxichrome) ── native messaging ── treetabs-host
                                                                        │
                       treetabs-gui (egui) ── Unix socket ── server ────┘
                                                               │
                                              store: tree.json + journal.jsonl
```

* **Extension** (`extension/`): thin. It forwards `tabs`, `windows` and
  `bookmarks` events, answers "resync" with all windows and tabs, and runs
  `chrome.*` calls for the host.
* **Without the companion app** the extension runs the same tree logic
  (`treetabs-sync::hub`) itself and keeps the tree in the browser's
  extension storage, with a daily copy (7 kept). There is no desktop window
  and no shared store then. Installing the app later hands the tree over
  on the next browser start. A new store takes it as its tree; otherwise
  it is kept as a backup. See [docs/OPTIONAL-HOST.md](docs/OPTIONAL-HOST.md).
* **Server** (`treetabs-sync`): the only process that writes the store. Its
  sync engine turns browser events into tree ops; the bookmark mirror diffs
  the tree against a `treetabs` bookmark folder. It runs in whichever
  process starts first, the native host or the GUI, and another process
  takes over if that one exits.
* **Store** (`treetabs-core::store`): every change is an op, fsynced to
  `journal.jsonl` before it counts, and compacted into `tree.json` (old
  snapshots go to `backups/`). Removed nodes go to a trash instead of being
  deleted.
* **Tree window**: the extension's toolbar button opens (or focuses) a
  popup window at the left screen edge with the tree.
  It is the same editor as the desktop app (`treetabs-ui`, egui), compiled
  to wasm (`extension/ui`). It reaches the server through the extension and
  the native host.
* **GUI** (`treetabs-gui`): the same editor as a desktop app, over the local
  socket. Drag and drop, undo/redo, trash with restore, search,
  open/focus/close in the browser.

### Behaviour

* Tabs opened from a tab are nested under it.
* Closing a tab moves it to the trash; its children move up.
* Closing a window keeps it as a saved window with saved tabs.
* After a browser restart, windows are matched to the stored ones by URLs
  (Chrome gives new ids every session), so there are no duplicates.
  Windows that vanished while nothing was watching become saved windows.
* Double-clicking a saved tab or window reopens it and binds the new browser
  tabs to the same nodes.
* Bookmarks: `Other bookmarks/treetabs` mirrors the tree. Windows and groups
  are folders. A tab with children is a folder holding the tab's own
  bookmark, then its children. Renaming, moving, deleting or adding
  bookmarks there changes the tree. Turn it off with
  `{"bookmarks": false}` in `<store>/settings.json`.
* Data tab: export the tree as JSON, bookmarks HTML or a Markdown list;
  import a JSON file (added as a group). Settings > Storage shows where the tree is kept
  and moves it to another folder; Settings > About.
* Backups: once a day the tree is exported as plain JSON to
  `<store>/exports/treetabs-YYYY-MM-DD.json`; the last 30 are kept
  (`{"daily_exports": 30}` in `<store>/settings.json`, 0 turns it off).
  The Backups tab of the tree window lists them and the store's own
  snapshots; "Add to tree" brings a copy back as a group of saved tabs
  (undoable, nothing is replaced).
* Trash: "Empty trash…" (asks first), and "Remove items older than"
  never / 7 / 30 / 90 / 365 days (`trash_days` in `<store>/settings.json`).
  Before anything is removed, today's backup is saved with it.
* Search: all words must match (title, custom title, URL or note); results
  show where each node is. Filters: open, saved, duplicates.
* Several browsers or profiles in one store: each window shows its
  browser's name when there is more than one, and the filter menu can show
  one profile only.
* The tree window opens where it was left, at the same size. Alt+T opens
  it (change in `chrome://extensions/shortcuts`).
* Languages: English, French, Ukrainian, Spanish, Portuguese, Finnish,
  German, Chinese, Russian, Arabic (Modern Standard) with Egyptian,
  Levantine, Maghrebi, Gulf and Iraqi variants (Settings > Look > Language;
  by default the system's). Translations are in
  `crates/treetabs-ui/i18n/*.json`, keyed by the English text; anything
  missing falls back to English (dialects first to Standard Arabic). A test
  checks that every text is translated in every full language.
  Chinese uses a subset of Noto Sans CJK SC, Arabic uses Noto Sans Arabic
  (`crates/treetabs-ui/fonts`, SIL OFL).
* Right to left: egui joins Arabic letters but doesn't apply the Unicode
  bidi rules, so `treetabs-ui/src/bidi.rs` reorders text before drawing
  (titles in any language, the whole interface in Arabic). In Arabic the
  bars are mirrored and the side panel is on the left; the tree keeps its
  left-to-right indentation. One-line text fields (search, rename,
  custom title) show typed Arabic in order, with the cursor where it
  belongs and the arrow keys moving visually; selecting part of mixed
  right-to-left text may highlight the wrong span.
* Settings > Look > Contrast: stronger text and icons. Settings > Size >
  Row button size: the rename/close/delete buttons, which sit at the right
  edge of the visible tree.

---

### Build

```sh
cargo build --release                 # treetabs, treetabs-host, treetabs-gui
cargo install cargo-oxichrome         # once
cargo xtask build-ext --release       # extension -> extension/dist/{chromium,firefox}
cargo xtask package                   # store zips, source zip, binaries -> dist/
```

### Install

See [docs/INSTALL.md](docs/INSTALL.md). In short:

```sh
cargo build --release && cargo xtask build-ext --release
target/release/treetabs install-host --browser chromium-snap --set-default
# then load extension/dist/chromium (or dist/firefox) in the browser
```

### CLI

```sh
treetabs convert <export.json | idb-folder> -o tree.json --pretty
treetabs convert tree.json -f tabs-outliner -o export.json  # write another format
treetabs import <export.json | idb-folder>   # start a new store from imported data
treetabs history <idb-folder> --recover out.json  # recover tabs from an older copy
treetabs live [--json]                       # print the live tree
treetabs open <node-id>                      # open/focus a node in the browser
treetabs serve                               # run the server in the foreground
treetabs revert --from-seq N [--apply]       # undo journal entries N.. (recovery)
treetabs info | idb-dump ...
```

### Tests

```sh
cargo test --workspace                       # unit + server integration tests
cd tests/e2e && npm install && npx playwright install chromium
npx playwright test                          # real Chromium with the extension
SNAP_TEST=1 SNAPS=chromium,brave,opera npx playwright test snap.spec.js  # snap browsers
FIREFOX_TEST=1 npx playwright test firefox.spec.js  # snap Firefox via geckodriver
```

#### Portable format

```json
{
  "format": "treetabs", "version": 1,
  "root": { "id": 1, "kind": "session", "children": [
    { "id": 2, "kind": "saved_window", "custom_title": "Work", "collapsed": true,
      "data": { "rect": "55_0_1920_1025" },
      "children": [ { "id": 3, "kind": "saved_tab", "title": "…", "url": "https://…" } ] }
  ] },
  "trash": [ { "removed_at": 1792890000000, "parent": 2, "index": 0, "node": { "…": "…" } } ]
}
```

`kind`: `window`, `saved_window`, `waiting_window`, `tab`, `saved_tab`,
`waiting_tab`, `attach_waiting_tab`, `group`, `note`, `separator`. 
Fields without a name of their own stay in `data`, `marks` and `extra`.


### Support

treetabs is free, with no ads and no paid features. If it is useful to
you, you can [donate with PayPal](https://www.paypal.com/donate/?hosted_button_id=TQTXU7ZB92CCL)
or scan the code below with your phone. In treetabs it is under
Settings > Support (the ♥ in the status bar). Settings > Credits lists
the projects treetabs is built on.

<img src="crates/treetabs-ui/assets/donate-qr.png" width="140" alt="QR code of the PayPal donation page for treetabs">

### License

See [LICENSE](LICENSE) for details.
