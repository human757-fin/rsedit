//! Fast Cutter installer — a "GitHub-native" Windows installer.
//!
//! It never ships the app itself; it only knows how to fetch the latest
//! official portable build from this repository's GitHub Releases and
//! install it into the user profile (no admin, no per-machine writes).

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const APP_NAME: &str = "Fast Cutter";
const OWNER: &str = "human757-fin";
const REPO: &str = "rsedit";
const GITHUB_API: &str = "https://api.github.com";
const UA: &str = "fast-cutter-installer/0.1";

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn install_dir() -> PathBuf {
    let base = env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into());
    PathBuf::from(base).join("Programs").join("FastCutter")
}

fn progress(window: &mut Instant, cur: u64, total: u64) {
    let seconds = window.elapsed().as_secs();
    if seconds == 0 {
        return;
    }
    let mb = 1024f64 * 1024f64;
    let dl = cur as f64 / mb;
    print!(
        "\r  Downloaded {:.1}/{:.1} MiB ({:.1} MiB/s);",
        dl,
        total as f64 / mb,
        dl / seconds as f64
    );
    let _ = std::io::stdout().flush();
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let resp = ureq::get(url)
        .header("User-Agent", UA)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let total: u64 = resp.body().content_length().unwrap_or(0);
    let mut reader = resp.into_body().into_reader();
    let mut out = fs::File::create(dest).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut got: u64 = 0;
    let mut clock = Instant::now();
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf)
            .map_err(|e| format!("read error: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("write error: {e}"))?;
        got += n as u64;
        if clock.elapsed() >= Duration::from_millis(200) {
            progress(&mut clock, got, total);
        }
    }
    println!();
    Ok(())
}

#[cfg(windows)]
fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    use std::io::copy;

    let file = fs::File::open(zip_path).map_err(|e| format!("cannot open {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("bad archive: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("entry: {e}"))?;
        let name = entry.name().replace('\\', "/");
        let Some(rel) = name.splitn(2, '/').nth(1) else {
            continue; // strip the top-level folder baked into the zip
        };
        let out_path = dest.join(rel);
        let parent = out_path.parent().unwrap();
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        if entry.is_dir() {
            fs::create_dir_all(&out_path).ok();
        } else {
            let mut f = fs::File::create(&out_path).map_err(|e| format!("create {}: {e}", out_path.display()))?;
            copy(&mut entry, &mut f).map_err(|e| format!("extract {}: {e}", out_path.display()))?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn extract_zip(_zip_path: &Path, _dest: &Path) -> Result<(), String> {
    Err("unzip is only supported on Windows".into())
}

#[cfg(windows)]
fn install_dir_write() -> Result<(), String> {
    let dir = install_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let exe = dir.join("FastCutter.exe");
    if !exe.exists() {
        return Err(format!("installed program missing: {}", exe.display()));
    }

    // Start Menu shortcut.
    let shell = "\
$ws = New-Object -ComObject WScript.Shell;
$lnk = $ws.CreateShortcut((Join-Path $env:APPDATA 'Microsoft\\Windows\\Start Menu\\Programs\\Fast Cutter.lnk'));
$lnk.TargetPath = $env:LOCALAPPDATA + '\\Programs\\FastCutter\\FastCutter.exe';
$lnk.WorkingDirectory = $env:LOCALAPPDATA + '\\Programs\\FastCutter';
$lnk.Save();";
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", shell])
        .status();

    // Uninstall registry entry (per-user).
    let uninstall_key_path = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\FastCutter";
    let hkcu = winreg::enums::HKEY_CURRENT_USER;
    let key = winreg::RegKey::predef(hkcu)
        .create_subkey(uninstall_key_path)
        .map_err(|e| format!("registry: {e}"))?
        .0;
    key.set_value("DisplayName", &APP_NAME).map_err(|e| format!("registry: {e}"))?;
    key.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION").to_string())
        .map_err(|e| format!("registry: {e}"))?;
    key.set_value("InstallLocation", &dir.display().to_string()).map_err(|e| format!("registry: {e}"))?;
    key.set_value("Publisher", &OWNER).map_err(|e| format!("registry: {e}"))?;
    let uninstall_cmd = format!(
        "{} --uninstall",
        env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "FastCutter-Setup.exe".into())
    );
    key.set_value("UninstallString", &uninstall_cmd).map_err(|e| format!("registry: {e}"))?;
    key.set_value("QuietUninstallString", &format!("{} --uninstall --silent", env::current_exe().map(|p| p.display().to_string()).unwrap_or_default())).map_err(|e| format!("registry: {e}"))?;
    key.set_value("NoModify", &1u32).map_err(|e| format!("registry: {e}"))?;
    key.set_value("NoRepair", &1u32).map_err(|e| format!("registry: {e}"))?;

    Ok(())
}

#[cfg(windows)]
fn remove_install() -> Result<(), String> {
    let dir = install_dir();
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
    }
    let _ = fs::remove_file(
        env::var_os("APPDATA")
            .map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\Fast Cutter.lnk"))
            .unwrap_or_default(),
    );
    let hkcu = winreg::enums::HKEY_CURRENT_USER;
    let key = winreg::RegKey::predef(hkcu)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall")
        .map_err(|e| format!("registry: {e}"))?;
    let _ = key.delete_subkey_all("FastCutter");
    Ok(())
}

#[cfg(not(windows))]
fn remove_install() -> Result<(), String> {
    Err("uninstall is only supported on Windows".into())
}

#[cfg(not(windows))]
fn install_dir_write() -> Result<(), String> {
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let silent = args.iter().any(|a| a == "--silent");
    let uninstall = args.iter().any(|a| a == "--uninstall");

    if uninstall {
        match remove_install() {
            Ok(()) => println!("Fast Cutter has been removed."),
            Err(e) => println!("Uninstall error: {e}"),
        }
        return;
    }

    println!();
    println!("  {APP_NAME} Installer");
    println!("  ───────────────────");
    println!("  Fetching latest release from github.com/{OWNER}/{REPO} …");

    let url = format!("{GITHUB_API}/repos/{OWNER}/{REPO}/releases/latest");
    let resp = match ureq::get(&url).header("User-Agent", UA).call() {
        Ok(r) => r,
        Err(e) => {
            println!("  Error: cannot reach GitHub: {e}");
            std::process::exit(1);
        }
    };
    let body = match resp.into_body().read_to_string() {
        Ok(b) => b,
        Err(e) => {
            println!("  Error: cannot read response: {e}");
            std::process::exit(1);
        }
    };

    let release: Release = serde_json::from_str(&body).unwrap_or_else(|e| {
        println!("  Error: cannot parse release data: {e}");
        std::process::exit(1);
    });

    let Some(asset) = release
        .assets
        .iter()
        .find(|a| a.name.contains("portable"))
    else {
        println!("  Error: no portable build found in the latest release.");
        std::process::exit(1);
    };

    println!("  Release: {}  ({} MiB)", release.tag_name, asset.size / (1024 * 1024));

    let tmp = env::temp_dir().join(format!("fastcutter-{}.zip", release.tag_name));
    println!("  Downloading {} …", asset.name);
    let windows = Instant::now();
    match download(&asset.browser_download_url, &tmp) {
        Ok(()) => {}
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            println!("  Error: {e}");
            std::process::exit(1);
        }
    }
    println!("  Downloaded in {:.1}s", std::time::Duration::from_secs_f64(windows.elapsed().as_secs_f64()).as_secs_f64());

    let dest = install_dir();
    println!("  Installing to {} …", dest.display());
    match extract_zip(&tmp, &dest) {
        Ok(()) => {}
        Err(e) => {
            println!("  Error: {e}");
            std::process::exit(1);
        }
    }

    if !silent {
        if let Err(e) = install_dir_write() {
            println!("  Warning: {e}");
        }
    }

    let _ = fs::remove_file(&tmp);

    println!();
    println!("  ✓ {APP_NAME} {} installed (v{})", release.tag_name, env!("CARGO_PKG_VERSION"));
    println!("    Run 'FastCutter.exe' via the Start Menu, or '{}'.", dest.display());
    if !silent {
        let _ = Command::new("explorer.exe").arg(&dest).spawn();
        println!("    Opening install folder in Explorer…");
    }
    println!();
}