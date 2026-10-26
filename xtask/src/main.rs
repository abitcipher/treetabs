//! Build helpers: `cargo xtask build-ext [--release] [--target chromium|firefox]` (both targets when none is given).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build-ext") => {
            let release = args.iter().any(|a| a == "--release");
            let targets: Vec<&str> = match args.iter().position(|a| a == "--target") {
                Some(i) => vec![args.get(i + 1).map(String::as_str).unwrap_or("chromium")],
                None => vec!["chromium", "firefox"],
            };
            for t in targets {
                build_ext(release, t)?;
            }
            Ok(())
        }
        Some("package") => package(),
        _ => bail!(
            "usage: cargo xtask build-ext [--release] [--target chromium|firefox] | cargo xtask package"
        ),
    }
}

/// Build the extension with oxichrome, then patch the manifest: add the
/// fixed `key` (stable extension id for the native host) and drop the
/// `web_accessible_resources` entry oxichrome adds (pages don't need our wasm).
fn build_ext(release: bool, target: &str) -> Result<()> {
    let firefox = match target {
        "chromium" => false,
        "firefox" => true,
        t => bail!("unknown target {t}: chromium or firefox"),
    };
    let ext = root().join("extension");
    let mut cmd = Command::new("cargo");
    cmd.args(["oxichrome", "build", "--target", target])
        .current_dir(&ext);
    if release {
        cmd.arg("--release");
    }
    if !cmd
        .status()
        .context("running cargo oxichrome (cargo install cargo-oxichrome)")?
        .success()
    {
        bail!("cargo oxichrome build failed");
    }
    // The tree window page: treetabs-ui compiled to wasm.
    let ui = ext.join("ui");
    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--target", "wasm32-unknown-unknown"])
        .current_dir(&ui);
    if release {
        cmd.arg("--release");
    }
    if !cmd.status()?.success() {
        bail!("building the tree window failed");
    }
    let wasm = ui
        .join("target/wasm32-unknown-unknown")
        .join(if release { "release" } else { "debug" })
        .join("treetabs_extension_ui.wasm");
    let dist = ext.join("dist").join(target);
    let out = dist.join("ui");
    let status = Command::new("wasm-bindgen")
        .args(["--target", "web", "--no-typescript", "--out-dir"])
        .arg(&out)
        .arg(&wasm)
        .status()
        .context("running wasm-bindgen")?;
    if !status.success() {
        bail!("wasm-bindgen failed for the tree window");
    }

    let manifest = dist.join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
    if let Some(o) = m.as_object_mut() {
        o.remove("web_accessible_resources");
    }
    if firefox {
        // Fixed add-on id: the native host manifest allows only this one.
        m["browser_specific_settings"] = serde_json::json!({"gecko": {"id": "treetabs@treetabs.dev", "strict_min_version": "128.0"}});
        // Chrome's favicon cache does not exist in Firefox. Site icons are
        // loaded from the sites, which needs this (off until the user turns
        // it on in the add-on's permissions).
        if let Some(p) = m["permissions"].as_array_mut() {
            p.retain(|v| v != "favicon");
        }
        m["optional_host_permissions"] = serde_json::json!(["<all_urls>"]);
        // Required for new add-ons on addons.mozilla.org: nothing leaves the computer (the tree goes only to the local native host).
        m["browser_specific_settings"]["gecko"]["data_collection_permissions"] =
            serde_json::json!({"required": ["none"]});
    } else {
        let key = std::fs::read_to_string(ext.join("key.txt"))?;
        m["key"] = serde_json::Value::String(key.trim().to_owned());
    }
    // From the logo pack in extension/static/icons (icon48 made from icon512).
    let icons = serde_json::json!({
        "16": "icons/icon16.png", "24": "icons/icon24.png", "32": "icons/icon32.png",
        "48": "icons/icon48.png", "64": "icons/icon64.png", "128": "icons/icon128.png"
    });
    // Toolbar button: a click opens the tree window (no popup).
    m["action"] = serde_json::json!({"default_title": "treetabs (Alt+T)", "default_icon": icons});
    m["icons"] = icons;
    // Keyboard shortcut for the toolbar button (opens the tree window); changeable in chrome://extensions/shortcuts.
    m["commands"] = serde_json::json!({
        "_execute_action": {"suggested_key": {"default": "Alt+T"}, "description": "Open the tree window"}
    });
    std::fs::write(&manifest, serde_json::to_vec_pretty(&m)?)?;
    println!("extension ready: {}", manifest.parent().unwrap().display());
    Ok(())
}

/// Release builds packed for the stores, in `dist/`:
/// - `treetabs-<v>-chrome-web-store.zip`: the Chromium build without the
///   `key` field (the Chrome Web Store refuses it and gives its own id);
///   also for Opera add-ons and Microsoft Edge Add-ons.
/// - `treetabs-<v>-chromium-unpacked.zip`: with the key (fixed id), for
///   loading unpacked or self-hosting.
/// - `treetabs-<v>-firefox.zip`: for addons.mozilla.org.
/// - `treetabs-<v>-source.zip`: the sources at HEAD (addons.mozilla.org
///   asks for them, since the add-on contains compiled wasm).
/// - `treetabs-<v>-linux-x86_64.tar.gz`: treetabs, treetabs-host, treetabs-gui.
fn package() -> Result<()> {
    let root = root();
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&root)
        .status()?;
    if !status.success() {
        bail!("cargo build --release failed");
    }
    for t in ["chromium", "firefox"] {
        build_ext(true, t)?;
    }
    // The extension's version (set in extension/src/lib.rs).
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(
        root.join("extension/dist/chromium/manifest.json"),
    )?)?;
    let version = manifest["version"].as_str().unwrap_or("0").to_owned();
    let out = root.join("dist");
    std::fs::create_dir_all(&out)?;
    let zip = |dir: &Path, name: &str| -> Result<PathBuf> {
        let file = out.join(name);
        let _ = std::fs::remove_file(&file);
        let ok = Command::new("zip")
            .args(["-qr", "-X"])
            .arg(&file)
            .arg(".")
            .current_dir(dir)
            .status()
            .context("running zip")?
            .success();
        if !ok {
            bail!("zip failed for {}", dir.display());
        }
        Ok(file)
    };
    let ext = root.join("extension/dist");
    let mut made = vec![zip(
        &ext.join("chromium"),
        &format!("treetabs-{version}-chromium-unpacked.zip"),
    )?];
    // Store copy: same files, manifest without "key".
    let store_dir = root.join("target/store-chromium");
    let _ = std::fs::remove_dir_all(&store_dir);
    copy_dir(&ext.join("chromium"), &store_dir)?;
    let mpath = store_dir.join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&mpath)?)?;
    if let Some(o) = m.as_object_mut() {
        o.remove("key");
    }
    std::fs::write(&mpath, serde_json::to_vec_pretty(&m)?)?;
    made.push(zip(
        &store_dir,
        &format!("treetabs-{version}-chrome-web-store.zip"),
    )?);
    made.push(zip(
        &ext.join("firefox"),
        &format!("treetabs-{version}-firefox.zip"),
    )?);
    let src = out.join(format!("treetabs-{version}-source.zip"));
    let ok = Command::new("git")
        .args(["archive", "--format=zip", "-o"])
        .arg(&src)
        .arg("HEAD")
        .current_dir(&root)
        .status()?
        .success();
    if !ok {
        bail!("git archive failed");
    }
    made.push(src);
    let bin = out.join(format!("treetabs-{version}-linux-x86_64.tar.gz"));
    let ok = Command::new("tar")
        .args(["-czf"])
        .arg(&bin)
        .args([
            "-C",
            "target/release",
            "treetabs",
            "treetabs-host",
            "treetabs-gui",
        ])
        .current_dir(&root)
        .status()?
        .success();
    if !ok {
        bail!("tar failed");
    }
    made.push(bin);
    for f in made {
        println!("{}", f.display());
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}
