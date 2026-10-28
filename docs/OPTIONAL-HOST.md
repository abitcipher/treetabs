# Plan: treetabs without the companion app

Status: implemented. Unit tests: `memstore`, `tests/hub.rs`, and the
mirror's `a_folder_of_another_tree_is_left_alone`. Browser tests:
`tests/e2e/standalone.spec.js`.

Goal: the extension works on its own. The companion app (`treetabs-host`)
becomes an upgrade, not a requirement. The tree logic stays one piece of
code; only where the tree is saved changes.

## Two modes

| | With the companion app (as now) | Without it (new) |
|---|---|---|
| Tree saved in | the store folder on disk | the browser's extension storage |
| Runs the tree logic | `treetabs-host` (or the GUI) | the extension's background script |
| Desktop window, `treetabs` command | yes | no |
| Several browsers share one tree | yes | no, each browser has its own |
| Daily copies | `<store>/exports/`, 30 kept | in browser storage, 7 kept |
| Bookmarks mirror | yes | yes |
| Move the store to another folder | yes | no |

**How the extension picks a mode, each time the background script starts:**

1. It tries the companion app (`connectNative`).
2. If the app answers, it uses the app, and remembers that it has seen it
   (`hostSeen` in `chrome.storage.local`).
3. If the browser says the app is not installed (Chrome: "native messaging
   host not found" or "forbidden", Firefox: "No such native application")
   and the app was never seen, it starts standalone.
4. If the app was seen before but is missing now, the extension does not
   start a second, empty tree. It keeps trying the app and shows that the
   app is not running. Starting a fresh tree here would silently split the
   user's data.

**From standalone to the app:** installing the app and restarting the
browser hands the standalone tree over. On the first connection, before its
window snapshot, the extension sends the whole tree (`adopt`).

- **Brand-new app store** (no journal entries, no nodes): the tree becomes
  the store.
- **The store already has a tree:** the standalone tree is saved as a
  backup in `<store>/exports/`. The status line says so, and Data → Backups
  can add it to the tree.

The extension also sends its bookmark map, the record of which bookmark
belongs to which node. Without it, the app would match the bookmark folder
to the tree by title and import the same tabs a second time. When the
standalone tree only becomes a backup, the app renames that folder to
"treetabs (kept in the browser)" and starts a new one, for the same
reason.

The app confirms the hand-over (`adopted`), and only then does the
extension delete its copy.

## Code changes

1. **Core (`treetabs-core`)**
   - A `TreeStore` trait: `tree`, `seq`, `apply_batch` and `adopt`.
     `Store` (files) implements it.
   - A new `MemStore` implements it too: the tree in memory, plus the
     journal entries and snapshots still to be written. The caller writes
     those wherever it keeps data.
   - Shared journal replay, so both stores load the same way.
   - Protocol:
     - `ClientMsg::AdoptTree`;
     - `ServerMsg::Adopted`;
     - `StoreOptions.kind` (folder or browser), so the UI can hide "Move".
2. **Sync (`treetabs-sync`)**
   - `server.rs` is split in two:
     - `hub.rs` holds the platform-free part: the message handling, the
       engine, the bookmarks bridge, settings, profiles and backups.
     - `server.rs` keeps the sockets and threads around it.
   - The hub talks to its surroundings through a `Backend` trait:
     - small named files (`settings.json`, `profiles.json`,
       `bookmarks.json`);
     - backups (list, read, write);
     - where the store is;
     - moving it.
     The native backend is the store folder.
   - The hub collects outgoing messages; the caller delivers them.
   - `bridge.rs` and `bookmarks.rs` use `web_time::Instant`, because
     `std::time::Instant` panics in wasm. The bookmark map is saved through
     the backend instead of a file path.
   - Sockets, threads, config and the host binary are built only for native
     targets, so the crate also builds for wasm.
   - The host passes `adopt` and `adopted` along.
3. **Extension**
   - It depends on `treetabs-sync` and runs the hub itself when standalone.
   - Its storage backend uses these keys:
     - `tt.tree`: the snapshot, with `journal_seq`;
     - `tt.j.<seq>`: one key per journal entry. A batch goes in one
       `storage.local.set`, which is atomic. Loading reads `seq + 1`,
       `seq + 2` and so on until a key is missing.
     - `tt.f.<name>`: small files;
     - `tt.exports` and `tt.x.<name>`: daily copies.
   - Writes go through one queue, in order. Everything waiting is written
     together.
   - A new snapshot is written after 2000 entries (as in the file store);
     then the old journal keys are removed.
   - Ticks: the bookmarks mirror every 250 ms, and the daily copy and trash
     expiry every minute.
   - **Keep-alive:** Chrome stops an idle background script after
     30 seconds. A missed event degrades the tree: a closed tab would be
     kept as "crash detected" instead of going to the trash, and a new tab
     would lose its place under its opener. The connection to the app keeps
     the script running today. Standalone, a cheap extension call every
     20 seconds does the same. When the script starts again anyway, the
     window snapshot sets the tree right, as after a host restart.
   - New permission: `unlimitedStorage`, because trees can be larger than
     the 10 MB default.
4. **UI**
   - Settings → Storage shows "In this browser" and no Move field.
   - The "host is not running" message names the companion app.
5. **Tests**
   - Unit tests:
     - `MemStore` replay and compaction;
     - adopting into a new and into a used store;
     - the hub without sockets.
   - Browser tests:
     - standalone, with a profile that has no host registered: tabs show
       in the tree, edits survive a restart, and bookmarks are mirrored;
     - hand-over: standalone first, then install the host and restart; the
       tree arrives in the host's store.
   - Every existing test must still pass, so behavior with the host stays
     the same.

## Switching by hand (added in 0.4.0)

Settings → Storage → *Use the companion app…* / *Stop using the
companion app…*. The choice is kept in `companionApp`, `"on"` or `"off"`
in `storage.local`. Without a choice, the extension picks the mode
automatically, as described above.

- **Off:** the app sends the tree, the bookmark map and its journal
  position (`Detached`). The extension keeps them (`tt.since`) and runs
  standalone without restarting.
- **On:** the extension saves everything, stops its hub and connects.
  The hand-over carries `since`. If the app's store has not moved on from
  that position, the tree replaces it; otherwise it becomes a backup.
- **Off while the app is not running:** a new tree, rebuilt from the
  bookmark folder.

## Systems without the app (added in 0.4.5)

The app exists only for Linux. At each start the background asks the
browser which system it runs on (`runtime.getPlatformInfo().os`; tests set
`testPlatform` in `storage.local` instead). Elsewhere it never starts the
app, ignores requests to use it, and tells each tree window
(`{"type":"companion_app","exists":false}`), which then shows no switch
and no note about the app in Settings → Storage. The check is
`treetabs_core::protocol::companion_app_exists`. If the browser does not
say, the extension behaves as before: it tries the app and falls back.

## Not in this step
- Daily copies as files in Downloads (needs the `downloads` permission).
- Syncing a standalone tree between computers.
