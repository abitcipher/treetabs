use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use treetabs_core::{idb, tabs_outliner};

#[derive(Parser)]
#[command(name = "treetabs", about = "Read Tabs Outliner data and convert it to portable JSON")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutFormat {
    /// treetabs portable JSON (nested tree)
    Portable,
    /// Tabs Outliner entry list (importable back into Tabs Outliner)
    TabsOutliner,
}

#[derive(Clone, Copy, ValueEnum)]
enum BrowserKind {
    Chromium,
    /// Chromium installed as a snap (Ubuntu).
    ChromiumSnap,
    Chrome,
    Brave,
    /// Brave installed as a snap.
    BraveSnap,
    Opera,
    /// Opera installed as a snap.
    OperaSnap,
    /// Firefox, also the snap (it starts hosts through the desktop portal,
    /// outside its sandbox, so the store can be shared).
    Firefox,
}

impl BrowserKind {
    /// Confined browsers can only run binaries and reach files in their
    /// own snap folder: the host copy and the store go there.
    fn snap(self) -> Option<&'static str> {
        match self {
            BrowserKind::ChromiumSnap => Some("chromium"),
            BrowserKind::BraveSnap => Some("brave"),
            BrowserKind::OperaSnap => Some("opera"),
            _ => None,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Convert a Tabs Outliner export, IndexedDB folder or portable file.
    Convert {
        /// Input: a .json file or an `*.indexeddb.leveldb` folder.
        input: PathBuf,
        /// Output file (stdout if omitted).
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long, value_enum, default_value = "portable")]
        format: OutFormat,
        /// Pretty-print the JSON.
        #[arg(short, long)]
        pretty: bool,
    },
    /// Show statistics about a tree.
    Info { input: PathBuf },
    /// List older versions of the Tabs Outliner tree still stored in an
    /// IndexedDB folder, and optionally recover tabs that went missing.
    History {
        dir: PathBuf,
        /// Write the current tree plus a "Recovered" section with tabs that
        /// exist only in older versions, as portable JSON.
        #[arg(long)]
        recover: Option<PathBuf>,
        /// Write version N (as numbered in the listing) as portable JSON.
        #[arg(long, requires = "output")]
        extract: Option<usize>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Register the native messaging host with a browser.
    InstallHost {
        #[arg(long, value_enum, default_value = "chromium")]
        browser: BrowserKind,
        /// Use this browser profile folder instead of the default location
        /// (for example a test profile).
        #[arg(long)]
        user_data_dir: Option<PathBuf>,
        /// Host binary (default: `treetabs-host` next to this program).
        #[arg(long)]
        host: Option<PathBuf>,
        /// Store folder (default: the configured one).
        #[arg(long)]
        store: Option<PathBuf>,
        /// Also write ~/.config/treetabs/config.json so the GUI uses this store.
        #[arg(long)]
        set_default: bool,
        /// Extension id allowed to start the host (Chromium browsers; repeat
        /// for several). Default: the id of the unpacked build. A store
        /// gives its own id: add it here.
        #[arg(long = "extension-id")]
        extension_ids: Vec<String>,
        /// Firefox add-on id allowed to start the host (repeatable).
        #[arg(long = "firefox-id")]
        firefox_ids: Vec<String>,
    },
    /// Print the live tree from the running server (starts one if needed).
    Live {
        #[arg(long)]
        store: Option<PathBuf>,
        /// Print portable JSON instead of an outline.
        #[arg(long)]
        json: bool,
    },
    /// Undo every journal entry from `--from-seq` on, through the running
    /// server (recovery tool). Shows what it would do unless `--apply`.
    Revert {
        #[arg(long)]
        from_seq: u64,
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
    },
    /// Run the server in the foreground (until stopped).
    Serve {
        #[arg(long)]
        store: Option<PathBuf>,
    },
    /// Open (or focus) a node in the connected browser.
    Open {
        id: u64,
        #[arg(long)]
        store: Option<PathBuf>,
    },
    /// Import a Tabs Outliner export / IndexedDB folder into the store.
    Import {
        input: PathBuf,
        #[arg(long)]
        store: Option<PathBuf>,
    },
    /// List the raw records of an IndexedDB folder (debugging aid).
    IdbDump {
        dir: PathBuf,
        /// Also print each decoded value (truncated to this many chars).
        #[arg(long)]
        values: Option<usize>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Convert { input, output, format, pretty } => convert(&input, output.as_deref(), format, pretty),
        Cmd::Info { input } => info(&input),
        Cmd::IdbDump { dir, values } => idb_dump(&dir, values),
        Cmd::InstallHost { browser, user_data_dir, host, store, set_default, extension_ids, firefox_ids } => {
            install_host(browser, user_data_dir, host, store, set_default, extension_ids, firefox_ids)
        }
        Cmd::Import { input, store } => import(&input, store),
        Cmd::Live { store, json } => live(store, json),
        Cmd::Open { id, store } => open(id, store),
        Cmd::Revert { from_seq, store, apply } => revert(from_seq, store, apply),
        Cmd::Serve { store } => {
            let dir = store.unwrap_or_else(treetabs_sync::config::store_dir);
            let _server = treetabs_sync::server::start(&dir)?;
            eprintln!("serving {}", dir.display());
            loop {
                std::thread::park();
            }
        }
        Cmd::History { dir, recover, extract, output } => history(&dir, recover.as_deref(), extract, output.as_deref()),
    }
}

fn load(input: &Path) -> Result<treetabs_core::Loaded> {
    let loaded = treetabs_core::load(input).with_context(|| format!("reading {}", input.display()))?;
    if !loaded.warnings.is_empty() {
        eprintln!("{} warning(s):", loaded.warnings.len());
        for w in loaded.warnings.iter().take(20) {
            eprintln!("  {w}");
        }
        if loaded.warnings.len() > 20 {
            eprintln!("  … {} more", loaded.warnings.len() - 20);
        }
    }
    Ok(loaded)
}

fn convert(input: &Path, output: Option<&Path>, format: OutFormat, pretty: bool) -> Result<()> {
    let loaded = load(input)?;
    let text = match format {
        OutFormat::Portable => treetabs_core::to_portable_json(&loaded.tree, pretty)?,
        OutFormat::TabsOutliner => {
            let v = tabs_outliner::to_value(&loaded.tree);
            if pretty { serde_json::to_string_pretty(&v)? } else { serde_json::to_string(&v)? }
        }
    };
    match output {
        Some(p) => {
            if p == input {
                bail!("refusing to overwrite the input file");
            }
            std::fs::write(p, text)?;
            eprintln!("wrote {} nodes to {}", loaded.tree.count() - 1, p.display());
        }
        None => println!("{text}"),
    }
    Ok(())
}

fn info(input: &Path) -> Result<()> {
    let loaded = load(input)?;
    let t = &loaded.tree;
    println!("source:   {:?}", loaded.format);
    println!("tree id:  {}", t.tree_id.as_deref().unwrap_or("-"));
    println!("saved at: {}", t.saved_at.map_or("-".into(), |v| v.to_string()));
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut max_depth = 0;
    t.root.walk(&mut |n, d| {
        *kinds.entry(n.kind.to_string()).or_default() += 1;
        max_depth = max_depth.max(d);
    });
    println!("nodes:    {} (max depth {max_depth})", t.count() - 1);
    for (k, n) in kinds {
        println!("  {k:<20} {n}");
    }
    Ok(())
}

fn idb_dump(dir: &Path, values: Option<usize>) -> Result<()> {
    let c = idb::read(dir)?;
    for (id, name) in &c.databases {
        println!("database {id}: {name}");
    }
    for r in &c.records {
        let status = match &r.value {
            Ok(_) => "ok".to_owned(),
            Err(e) => format!("ERROR {e}"),
        };
        println!(
            "db {} store {} ({}) key {} — {} bytes, {status}",
            r.database_id,
            r.store_id,
            r.store_name.as_deref().unwrap_or("?"),
            r.key,
            r.raw_len
        );
        if let (Some(n), Ok(v)) = (values, &r.value) {
            let s = v.to_string();
            let cut: String = s.chars().take(n).collect();
            println!("    {cut}{}", if s.chars().count() > n { "…" } else { "" });
        }
    }
    for w in &c.warnings {
        eprintln!("warning: {w}");
    }
    Ok(())
}

fn history(dir: &Path, recover: Option<&Path>, extract: Option<usize>, output: Option<&Path>) -> Result<()> {
    use treetabs_core::recover::{lost_tabs, tab_urls};
    let versions = idb::snapshot_history(dir)?;
    let Some(current) = versions.iter().find_map(|v| v.tree.as_ref().ok()) else {
        bail!("no readable snapshot version found");
    };
    let current_urls = tab_urls(current);
    println!("{:>3}  {:>12}  {:>8}  {:>6}  {:>13}  missing-now", "#", "seq", "nodes", "tabs", "saved_at");
    for (i, v) in versions.iter().enumerate() {
        match &v.tree {
            Ok(t) => {
                let urls = tab_urls(t);
                let missing = urls.iter().filter(|u| !current_urls.contains(*u)).count();
                println!(
                    "{i:>3}  {:>12}  {:>8}  {:>6}  {:>13}  {missing}{}",
                    v.seq,
                    t.count() - 1,
                    urls.len(),
                    t.saved_at.map_or("-".into(), |s| s.to_string()),
                    if v.is_current { "  (current)" } else { "" }
                );
            }
            Err(e) => println!("{i:>3}  {:>12}  unreadable: {e}", v.seq),
        }
    }
    if let Some(n) = extract {
        let v = versions.get(n).with_context(|| format!("no version {n}"))?;
        let t = v.tree.as_ref().map_err(|e| anyhow::anyhow!("version {n} unreadable: {e}"))?;
        let out = output.expect("clap requires output");
        treetabs_core::save_portable(t, out, true)?;
        eprintln!("wrote version {n} to {}", out.display());
    }
    if let Some(out) = recover {
        let older = versions.iter().filter(|v| !v.is_current).filter_map(|v| v.tree.as_ref().ok());
        let found = lost_tabs(current, older, |_, t| {
            format!("Recovered (version saved {})", t.saved_at.map_or("?".into(), |s| s.to_string()))
        });
        let n: usize = found.iter().map(|w| w.children.len()).sum();
        let mut tree = current.clone();
        let mut group = treetabs_core::Node::new(treetabs_core::NodeKind::Group);
        group.custom_title = Some(format!("Recovered tabs ({n})"));
        group.children = found;
        if n > 0 {
            tree.root.children.push(group);
        }
        treetabs_core::save_portable(&tree, out, true)?;
        eprintln!("recovered {n} tab(s) missing from the current tree; wrote {}", out.display());
    }
    Ok(())
}

const HOST_NAME: &str = "dev.treetabs.host";
const EXTENSION_ID: &str = "ipglemijkiaehilbokjjppicafjcbcbg";
/// Firefox add-on id (`browser_specific_settings.gecko.id`).
const FIREFOX_ID: &str = "treetabs@treetabs.dev";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).expect("HOME is set")
}

fn install_host(
    browser: BrowserKind,
    user_data_dir: Option<PathBuf>,
    host: Option<PathBuf>,
    store: Option<PathBuf>,
    set_default: bool,
    extension_ids: Vec<String>,
    firefox_ids: Vec<String>,
) -> Result<()> {
    let extension_ids = if extension_ids.is_empty() { vec![EXTENSION_ID.to_owned()] } else { extension_ids };
    let firefox_ids = if firefox_ids.is_empty() { vec![FIREFOX_ID.to_owned()] } else { firefox_ids };
    use treetabs_sync::config::{Config, host_config_path, store_dir, user_config_path};
    let exe = std::env::current_exe()?;
    let mut host = host.unwrap_or_else(|| exe.with_file_name("treetabs-host"));
    if !host.exists() {
        bail!("host binary {} not found (cargo build -p treetabs-sync)", host.display());
    }
    host = host.canonicalize()?;
    let mut store = store.unwrap_or_else(store_dir);
    if let Some(snap) = browser.snap() {
        let snap_common = home().join("snap").join(snap).join("common");
        let dir = snap_common.join("treetabs");
        std::fs::create_dir_all(&dir)?;
        let copy = dir.join("treetabs-host");
        // Copy then rename: the old binary may be running right now.
        let tmp = dir.join("treetabs-host.new");
        std::fs::copy(&host, &tmp)?;
        std::fs::rename(&tmp, &copy)?;
        host = copy;
        if !store.starts_with(&snap_common) {
            store = dir.join("store");
        }
    }
    std::fs::create_dir_all(&store)?;
    let store = store.canonicalize()?;
    let hosts_dir = match (user_data_dir, browser) {
        (Some(d), _) => d.join("NativeMessagingHosts"),
        (None, BrowserKind::Chromium) => home().join(".config/chromium/NativeMessagingHosts"),
        (None, BrowserKind::ChromiumSnap) => home().join("snap/chromium/common/chromium/NativeMessagingHosts"),
        (None, BrowserKind::Chrome) => home().join(".config/google-chrome/NativeMessagingHosts"),
        (None, BrowserKind::Brave) => home().join(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        // The snap's own home; Brave ignores --user-data-dir for hosts.
        (None, BrowserKind::BraveSnap) => home().join("snap/brave/current/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        // Opera reads Chrome's location (not its own config folder).
        (None, BrowserKind::Opera) => home().join(".config/google-chrome/NativeMessagingHosts"),
        (None, BrowserKind::OperaSnap) => home().join("snap/opera/current/.config/google-chrome/NativeMessagingHosts"),
        // Read by Firefox, and by the portal for the snap.
        (None, BrowserKind::Firefox) => home().join(".mozilla/native-messaging-hosts"),
    };
    std::fs::create_dir_all(&hosts_dir)?;
    let manifest = match browser {
        BrowserKind::Firefox => serde_json::json!({
            "name": HOST_NAME,
            "description": "treetabs native host",
            "path": host,
            "type": "stdio",
            "allowed_extensions": firefox_ids,
        }),
        _ => serde_json::json!({
            "name": HOST_NAME,
            "description": "treetabs native host",
            "path": host,
            "type": "stdio",
            "allowed_origins": extension_ids.iter().map(|id| format!("chrome-extension://{id}/")).collect::<Vec<_>>(),
        }),
    };
    let mpath = hosts_dir.join(format!("{HOST_NAME}.json"));
    std::fs::write(&mpath, serde_json::to_vec_pretty(&manifest)?)?;
    let cfg = Config { store_dir: Some(store.clone()) };
    std::fs::write(host_config_path(&host), serde_json::to_vec_pretty(&cfg)?)?;
    println!("host manifest: {}", mpath.display());
    println!("host binary:   {}", host.display());
    println!("store:         {}", store.display());
    if set_default {
        let p = user_config_path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, serde_json::to_vec_pretty(&cfg)?)?;
        println!("default store set in {}", p.display());
    }
    Ok(())
}

fn import(input: &Path, store: Option<PathBuf>) -> Result<()> {
    let loaded = load(input)?;
    let dir = store.unwrap_or_else(treetabs_sync::config::store_dir);
    let n = loaded.tree.count() - 1;
    treetabs_core::Store::create(&dir, loaded.tree).with_context(|| format!("creating store in {}", dir.display()))?;
    eprintln!("imported {n} nodes into {}", dir.display());
    Ok(())
}

fn live(store: Option<PathBuf>, json: bool) -> Result<()> {
    use treetabs_sync::{Role, ServerMsg};
    let dir = store.unwrap_or_else(treetabs_sync::config::store_dir);
    let conn = treetabs_sync::connect(&dir, Role::Viewer, "cli", || {})?;
    let tree = loop {
        match conn.rx.recv_timeout(std::time::Duration::from_secs(10))? {
            ServerMsg::Snapshot { tree, .. } => break tree,
            _ => continue,
        }
    };
    if json {
        println!("{}", treetabs_core::to_portable_json(&tree, true)?);
        return Ok(());
    }
    tree.root.walk(&mut |n, d| {
        if d == 0 {
            return;
        }
        let url = n.url.as_deref().map(|u| format!("  <{u}>")).unwrap_or_default();
        println!("{}{} {}{url}", "  ".repeat(d - 1), n.kind, n.display_title());
    });
    if !tree.trash.is_empty() {
        println!("(trash: {} item(s))", tree.trash.len());
    }
    Ok(())
}

fn open(id: u64, store: Option<PathBuf>) -> Result<()> {
    use treetabs_sync::{ClientMsg, Role};
    let dir = store.unwrap_or_else(treetabs_sync::config::store_dir);
    // The browser's host may still be starting: wait for it a little.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let conn = loop {
        let conn = treetabs_sync::connect(&dir, Role::Viewer, "cli", || {})?;
        let browsers = if conn.is_server() {
            0
        } else {
            let mut n = 0;
            while let Ok(m) = conn.rx.recv_timeout(std::time::Duration::from_millis(500)) {
                if let treetabs_sync::ServerMsg::Status { browsers, .. } = m {
                    n = browsers;
                    break;
                }
            }
            n
        };
        if browsers > 0 {
            break conn;
        }
        drop(conn);
        if std::time::Instant::now() > deadline {
            bail!("no browser connected");
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    };
    conn.send(&ClientMsg::Activate { id: treetabs_core::NodeId(id) })?;
    // Give the server a moment to read it before we hang up.
    std::thread::sleep(std::time::Duration::from_millis(300));
    Ok(())
}

fn revert(from: u64, store: Option<PathBuf>, apply: bool) -> Result<()> {
    use std::io::BufRead;
    use treetabs_core::store::JournalEntry;
    use treetabs_sync::{ClientMsg, Role, ServerMsg};
    let dir = store.unwrap_or_else(treetabs_sync::config::store_dir);
    let mut tree: treetabs_core::Tree = serde_json::from_slice(&std::fs::read(dir.join("tree.json"))?)?;
    let base = tree.journal_seq.unwrap_or(0);
    if from <= base {
        bail!("entry {from} is already part of the snapshot (journal starts after {base})");
    }
    let file = std::fs::File::open(dir.join("journal.jsonl"))?;
    let mut later = Vec::new();
    let mut last = base;
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let e: JournalEntry = serde_json::from_str(&line)?;
        if e.seq <= base {
            continue;
        }
        last = e.seq;
        if e.seq < from {
            tree.apply(&e.op).map_err(|err| anyhow::anyhow!("replaying {}: {err}", e.seq))?;
        } else {
            later.push(e);
        }
    }
    let mut inverse = Vec::new();
    for e in &later {
        match tree.apply(&e.op).map_err(|err| anyhow::anyhow!("replaying {}: {err}", e.seq))? {
            Some(inv) => inverse.push(inv),
            None => bail!("entry {} ({}) cannot be undone", e.seq, e.op.name()),
        }
    }
    inverse.reverse();
    eprintln!("{} entries ({from}..={last}) -> {} inverse ops", later.len(), inverse.len());
    if !apply {
        eprintln!("dry run; add --apply to send them");
        return Ok(());
    }
    let conn = treetabs_sync::connect(&dir, Role::Viewer, "revert", || {})?;
    // Make sure nothing happened after what we read.
    loop {
        match conn.rx.recv_timeout(std::time::Duration::from_secs(10))? {
            ServerMsg::Snapshot { seq, .. } if seq != last => bail!("store moved on (seq {seq}, read {last}); run again"),
            ServerMsg::Snapshot { .. } => break,
            _ => {}
        }
    }
    conn.send(&ClientMsg::Edit { req: 1, ops: inverse })?;
    loop {
        if let ServerMsg::EditResult { err, .. } = conn.rx.recv_timeout(std::time::Duration::from_secs(10))? {
            match err {
                Some(e) => bail!("server refused: {e}"),
                None => {
                    eprintln!("reverted");
                    return Ok(());
                }
            }
        }
    }
}
