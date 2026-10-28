# treetabs user guide: with and without the companion app

treetabs comes in one extension that works in two ways:

* **On its own (the default).** You install only the browser extension.
  The tree is kept inside the browser.
* **With the companion app (Linux only).** You also install a small
  program, `treetabs-host`, on your computer. The tree is then kept in a
  folder on disk, where the desktop window, the `treetabs` command and
  other browsers can use it too.

On Linux you can switch between the two at any time from the tree window
(Settings → Storage), and the tree comes along. On Windows, macOS and
other systems, treetabs works on its own, and Settings → Storage has no
switch. This guide explains how
to install each version, what changes between them, and how to turn the
app on and off.

For the tree window itself (rows, search, trash, the Data tab, settings),
see [GUIDE.md](GUIDE.md). For building from source, see
[INSTALL.md](INSTALL.md).

---

## 1. Which one do I need?

| | On its own (default) | With the companion app |
|---|---|---|
| What you install | the extension | the extension + `treetabs-host` (Linux) |
| Where the tree is kept | the browser's extension storage, in this browser profile | a folder on your computer (the *store*) |
| Tabs, nesting, trash, notes, groups, search, undo | yes | yes |
| Mirror in the "treetabs" bookmark folder | yes | yes |
| Export and import files (Data tab) | yes | yes |
| Daily copies (Data → Backups) | 7, kept in the browser | 30, as JSON files in the store |
| Survives removing the extension or resetting the browser | **no**: export or use the bookmark mirror | yes, the folder stays |
| Desktop window (`treetabs-gui`) and `treetabs` command | no | yes |
| Several browsers or profiles in one tree | no, each has its own tree | yes |
| Move the tree to another folder | no | yes (Settings → Storage) |
| Every change written to disk before it counts | written to browser storage, in order | yes, to a journal on disk |

**Start on its own** if you only use one browser and want nothing to
install. **Add the app** if you want the tree in a folder you control,
the desktop window, or one tree shared by several browsers.

---

## 2. The default: the extension on its own

### Install

**From a store** (Chrome Web Store, Firefox Add-ons), when published:
click *Add to browser*. That is all.

**From the release package** (`treetabs-0.4.6-chromium-unpacked.zip`),
for Chromium, Chrome, Brave, Opera or Edge:

1. Unzip it into a folder you keep. For a **snap** browser, use a folder
   under `~/snap/<browser>/common/`, for example
   `~/snap/chromium/common/treetabs/extension`, because snaps cannot read
   other folders.
2. Open `chrome://extensions` (Opera: `opera://extensions`) and turn on
   *Developer mode*.
3. Click *Load unpacked* and choose the folder.

**Firefox** (`treetabs-0.4.6-firefox.zip`): open
`about:debugging#/runtime/this-firefox`, click *Load Temporary Add-on…*
and choose the `manifest.json` inside. A temporary add-on is removed when
Firefox closes; the signed version from addons.mozilla.org stays.

### First start

Click the treetabs button in the toolbar, or press **Alt+T**. The tree
window opens with your open windows and tabs. Settings → Storage says
**"In this browser's own storage"**. That is how you know the extension
works on its own.

### Using it

Everything in [GUIDE.md](GUIDE.md) works the same. Some things to know:

* **Your tree lives in this browser profile.** Another profile or another
  browser has its own tree.
* **Removing the extension deletes the tree**, and so does resetting the
  browser or clearing the extension's data. Two things protect you:
  * the bookmark folder `Other bookmarks/treetabs` mirrors the tree;
  * **Data → Export…** saves the whole tree as a file. Do this now and then,
    or before big changes. **Import file…** brings it back.
* **Daily copies** (Data → Backups) are kept in the browser too, so they
  go with it. Seven are kept.
* The extension keeps its background part running while the browser is
  open. If it stopped, Chrome would not tell it about tabs closed
  meanwhile. It makes one tiny, local call every 20 seconds, and sends
  nothing anywhere.

---

## 3. With the companion app

### Install the app

The app is for Linux. Download `treetabs-0.4.6-linux-x86_64.tar.gz` and
unpack it. It holds three programs: `treetabs` (command line),
`treetabs-host` (the app the browser starts) and `treetabs-gui` (desktop
window). Keep them in a folder that stays, for example `~/.local/bin`.

Register the app with your browser:

```sh
./treetabs install-host --browser chromium-snap --set-default
```

`--browser` is one of the following. If you have several browsers, run
the command once for each.

| `--browser` | Browser |
|---|---|
| `chromium` | Chromium (deb or distribution package) |
| `chromium-snap` | Chromium snap (Ubuntu's default) |
| `chrome` | Google Chrome |
| `brave`, `brave-snap` | Brave |
| `opera`, `opera-snap` | Opera |
| `firefox` | Firefox, deb or snap |

* `--set-default` lets the desktop window and the `treetabs` command use
  the same tree.
* **If the extension came from a store**, add its id:
  `--extension-id <id>`. `chrome://extensions` shows the id under the
  extension's name. Without it, the browser does not allow the extension
  to start the app.
* **Snap browsers** can only start programs inside their own snap
  folder. `install-host` copies the app to `~/snap/<browser>/common/treetabs/`
  and keeps the tree there, in `…/treetabs/store`.
* **Firefox snap** asks once whether Firefox may start
  `dev.treetabs.host`: answer *Allow*.

### Turn it on

Either way works:

* **Restart the browser.** If you never turned the app off, the extension
  uses it as soon as it finds it.
* **Or, without restarting:** tree window → ⚙ Settings → Storage →
  **Use the companion app…** → **Use the app**.

What happens to your tree:

| The app's store | Then |
|---|---|
| is new (empty) | your tree moves into it |
| is the one you left with *Stop using the companion app*, unchanged since | your tree replaces it: the changes you made in the browser are kept |
| holds another tree (or was used since you left it) | the app's tree stays. Yours is saved as a backup named `from-browser-….json`: Data → Backups → *Add to tree* puts it back as a group. The old bookmark folder is renamed **"treetabs (kept in the browser)"**, so nothing is imported twice |

Once the app has the tree, the extension deletes its own copy.

If the app is not installed when you ask for it, the status bar says so
and the tree stays in the browser. The extension tries again at every
browser start.

### Using it

* Settings → Storage shows the store folder and can **move** it.
* The desktop window: `treetabs-gui`. It shows the same tree, live.
* The command line: `treetabs live` prints the tree, and `treetabs --help`
  lists everything else.
* Several browsers can use the same store. Each window is labelled with
  its browser, and the filter menu can show one browser only.
* The app writes a log to `<store>/host.log`.

Where things are:

| What | Where |
|---|---|
| The tree | `<store>/tree.json` and `<store>/journal.jsonl` |
| Daily copies | `<store>/exports/` (the last 30) |
| Snapshots | `<store>/backups/` |
| Store options | `<store>/settings.json` |

`<store>` is `~/.local/share/treetabs`, or for a snap browser
`~/snap/<browser>/common/treetabs/store`. Settings → Storage shows it.

---

## 4. Turning the app off

Tree window → ⚙ Settings → Storage → **Stop using the companion app…** →
**Stop using the app**.

* The app sends the whole tree (with trash, notes and the bookmark
  mapping) to the extension, which keeps it in the browser from now on.
  The tree window stays open and keeps working.
* The app's folder **stays as it is**, a frozen copy of the tree at that
  moment. Nothing is deleted.
* The extension does not start the app any more, even though it is still
  installed, until you choose *Use the companion app…* again.
* The desktop window and the `treetabs` command still open the store
  folder, but they no longer see what happens in this browser.

**To remove the app completely**, first turn it off as above, then
delete its registration:

* the file `dev.treetabs.host.json` in the browser's folder (the table in
  [INSTALL.md](INSTALL.md#3-install-the-native-host) lists them, for
  example `~/snap/chromium/common/chromium/NativeMessagingHosts/`);
* the programs, if you like;
* the store folder only if you no longer want that copy of the tree.

**If you removed the app before turning it off**, the extension does not
silently start a new, empty tree. It waits, and the status bar says *The
treetabs companion app is not running…*. Then either:

* install the app again, and everything is as before; or
* choose Settings → Storage → **Stop using the companion app…** anyway.
  Without the app nothing can be sent along, so the new tree in the
  browser is **rebuilt from the bookmark folder**: windows, groups and
  notes come back as folders, tabs as saved tabs. The trash, note texts
  and window positions are not in the bookmarks. To keep them, take the
  store's daily copy (`<store>/exports/treetabs-<date>.json`) and use
  Data → **Import file…**.

---

## 5. Quick reference

| I want to… | Do this |
|---|---|
| try treetabs | install the extension only |
| keep the tree in a folder, use the desktop window | install the app, then *Use the companion app…* (or restart the browser) |
| go back to the extension alone | *Stop using the companion app…* |
| have a copy I control in either version | Data → *Export…* |
| bring back an older state | Data → Backups → *Add to tree* |
| see which version is running | Settings → Storage: "In this browser's own storage", or a folder path |

## 6. Troubleshooting

| What you see | What to do |
|---|---|
| *The treetabs companion app is not running…* | The app was used before but cannot be started. Reinstall it (section 3), or turn it off (section 4). |
| *The companion app is not installed: the tree stays in the browser* | You asked for the app, but the browser cannot find it. Check the `--browser` value, and `--extension-id` for a store install. |
| The app is installed but not used | You turned it off earlier. Choose *Use the companion app…*. |
| The same tabs appear twice after switching | Check Data → Backups for `from-browser-…` and the bookmark folder "treetabs (kept in the browser)". Delete the extra group with Ctrl+Z, or move it to the trash. |
| Errors | `chrome://extensions` → treetabs → *Inspect views: service worker* shows the console. With the app, also see `<store>/host.log`. |
