//! rsedit Installer — a small GUI that fetches the latest official
//! portable build from this repository's GitHub Releases and installs it
//! for the current user only (no admin, no per-machine writes).
//!
//! It also manages an applications-menu (Start menu) shortcut and an
//! optional desktop shortcut, both toggleable from the UI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

const APP_NAME: &str = "rsedit";
const OWNER: &str = "human757-fin";
const REPO: &str = "rsedit";
const GITHUB_API: &str = "https://api.github.com";
const UA: &str = "rsedit-installer/0.1";

const BG: egui::Color32 = egui::Color32::from_rgb(12, 12, 14);
const CARD: egui::Color32 = egui::Color32::from_rgb(18, 18, 22);
const CARD_2: egui::Color32 = egui::Color32::from_rgb(24, 24, 30);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(84, 130, 220);
const ACCENT_DARK: egui::Color32 = egui::Color32::from_rgb(48, 82, 150);
const WHITE: egui::Color32 = egui::Color32::from_rgb(236, 236, 240);
const GRAY: egui::Color32 = egui::Color32::from_rgb(168, 172, 180);
const DIM: egui::Color32 = egui::Color32::from_rgb(108, 112, 120);
const GREEN: egui::Color32 = egui::Color32::from_rgb(96, 200, 130);
const RED: egui::Color32 = egui::Color32::from_rgb(226, 96, 96);

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

// ---------------------------------------------------------------------------
// Platform helpers
// ---------------------------------------------------------------------------

fn home() -> PathBuf {
    let Some(h) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) else {
        return PathBuf::from(".");
    };
    PathBuf::from(h)
}

fn default_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into());
        PathBuf::from(base).join("Programs").join("rsedit")
    }
    #[cfg(not(windows))]
    {
        home().join(".local").join("share").join("rsedit")
    }
}

fn exe_name() -> &'static str {
    #[cfg(windows)]
    {
        "rsedit.exe"
    }
    #[cfg(not(windows))]
    {
        "rsedit"
    }
}

fn installed_version(dir: &Path) -> Option<String> {
    let v = fs::read_to_string(dir.join("version.txt")).ok()?;
    let v = v.trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

#[cfg(windows)]
fn run_powershell(script: &str) -> Result<(), String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "powershell exited {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(windows)]
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(windows)]
fn desktop_folder() -> Result<PathBuf, String> {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetFolderPath('Desktop')",
        ])
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        let base = env::var_os("USERPROFILE").ok_or("no USERPROFILE")?;
        Ok(PathBuf::from(base).join("Desktop"))
    } else {
        Ok(PathBuf::from(s))
    }
}

#[cfg(not(windows))]
fn data_home() -> PathBuf {
    if let Ok(d) = env::var("XDG_DATA_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    home().join(".local").join("share")
}

#[cfg(not(windows))]
fn desktop_dir() -> PathBuf {
    if let Ok(d) = env::var("XDG_DESKTOP_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    home().join("Desktop")
}

// Embedded app icon (small PNG shipped with the portable build).
#[cfg(not(windows))]
const ICON_PNG: &[u8] = include_bytes!("../../packaging/rsedit.png");

// Shortcut locations ---------------------------------------------------------

#[cfg(windows)]
fn start_menu_lnk() -> Result<PathBuf, String> {
    let apps = env::var_os("APPDATA")
        .ok_or("no APPDATA")?
        .to_string_lossy()
        .to_string();
    Ok(PathBuf::from(apps)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("rsedit.lnk"))
}

#[cfg(not(windows))]
fn app_menu_file() -> PathBuf {
    data_home().join("applications").join("rsedit.desktop")
}

#[cfg(windows)]
fn desktop_lnk() -> Result<PathBuf, String> {
    Ok(desktop_folder()?.join("rsedit.lnk"))
}

fn desktop_shortcut_file() -> PathBuf {
    #[cfg(windows)]
    {
        desktop_lnk().unwrap_or_else(|_| home().join("Desktop").join("rsedit.lnk"))
    }
    #[cfg(not(windows))]
    {
        desktop_dir().join("rsedit.desktop")
    }
}

// ---------------------------------------------------------------------------
// GitHub release fetch
// ---------------------------------------------------------------------------

fn fetch_latest_release() -> Result<Release, String> {
    let url = format!("{GITHUB_API}/repos/{OWNER}/{REPO}/releases/latest");
    let resp = ureq::get(&url).header("User-Agent", UA).call().map_err(|e| {
        format!("cannot reach GitHub: {e}")
    })?;
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("cannot read response: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("cannot parse release data: {e}"))
}

fn asset_pick(name: &str) -> bool {
    #[cfg(windows)]
    {
        name.contains("windows-portable")
    }
    #[cfg(not(windows))]
    {
        name.ends_with(".tar.gz") && name.contains("linux-x86_64")
    }
}

fn download_to(url: &str, dest: &Path, sh: Shared) -> Result<(), String> {
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
    let mut last = 0u64;
    loop {
        if sh.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let n = std::io::Read::read(&mut reader, &mut buf)
            .map_err(|e| format!("read error: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("write error: {e}"))?;
        got += n as u64;
        if clock.elapsed() >= Duration::from_millis(150) {
            let t = clock.elapsed().as_secs_f64();
            let mbps = if t > 0.0 {
                (got - last) as f64 / (1024.0 * 1024.0) / t
            } else {
                0.0
            };
            last = got;
            clock = Instant::now();
            set_phase(&sh, Phase::Downloading { got, total, mbps });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn extract_archive(archive: &Path, dest: &Path) -> Result<(), String> {
    use std::io::copy;

    fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    let file = fs::File::open(archive).map_err(|e| format!("cannot open {}: {e}", archive.display()))?;
    let mut z = zip::ZipArchive::new(file).map_err(|e| format!("bad archive: {e}"))?;
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| format!("entry: {e}"))?;
        let name = entry.name().replace('\\', "/");
        let Some(rel) = name.splitn(2, '/').nth(1) else {
            continue; // strip the top-level folder baked into the zip
        };
        if rel.is_empty() {
            continue;
        }
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
fn extract_archive(archive: &Path, dest: &Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::io::copy;

    fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    let file = fs::File::open(archive).map_err(|e| format!("cannot open {}: {e}", archive.display()))?;
    let gz = GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    let entries = tar
        .entries()
        .map_err(|e| format!("bad archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("entry: {e}"))?;
        let name = entry
            .path()
            .map_err(|e| format!("entry path: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        let Some(rel) = name.splitn(2, '/').nth(1) else {
            continue; // strip the top-level folder
        };
        if rel.is_empty() {
            continue;
        }
        let out_path = dest.join(rel);
        let parent = out_path.parent().unwrap();
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&out_path).ok();
        } else if kind.is_file() {
            let mut f = fs::File::create(&out_path).map_err(|e| format!("create {}: {e}", out_path.display()))?;
            copy(&mut entry, &mut f).map_err(|e| format!("extract {}: {e}", out_path.display()))?;
        }
    }
    // Ship the app menu icon alongside the binary.
    fs::write(dest.join("icon.png"), ICON_PNG)
        .map_err(|e| format!("cannot write icon.png: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shortcuts
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn write_start_menu(dir: &Path) -> Result<(), String> {
    let target = dir.join(exe_name());
    let lnk = start_menu_lnk()?;
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell;\n\
         $lnk = $ws.CreateShortcut({});\n\
         $lnk.TargetPath = {};\n\
         $lnk.WorkingDirectory = {};\n\
         $lnk.IconLocation = {};\n\
         $lnk.Description = 'rsedit video editor';\n\
         $lnk.Save();",
        ps_quote(&lnk.display().to_string()),
        ps_quote(&target.display().to_string()),
        ps_quote(&dir.display().to_string()),
        ps_quote(&target.display().to_string()),
    );
    run_powershell(&script)
}

#[cfg(not(windows))]
fn write_start_menu(dir: &Path) -> Result<(), String> {
    let entry = desktop_entry_body(dir, &dir.join("icon.png"));
    let file = app_menu_file();
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    fs::write(&file, entry).map_err(|e| format!("cannot write {}: {e}", file.display()))
}

#[cfg(windows)]
fn write_desktop(dir: &Path) -> Result<(), String> {
    let target = dir.join(exe_name());
    let lnk = desktop_folder()?.join("rsedit.lnk");
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell;\n\
         $lnk = $ws.CreateShortcut({});\n\
         $lnk.TargetPath = {};\n\
         $lnk.WorkingDirectory = {};\n\
         $lnk.IconLocation = {};\n\
         $lnk.Save();",
        ps_quote(&lnk.display().to_string()),
        ps_quote(&target.display().to_string()),
        ps_quote(&dir.display().to_string()),
        ps_quote(&target.display().to_string()),
    );
    run_powershell(&script)?;
    let _ = Command::new("rundll32.exe")
        .args(["user32.dll,UpdatePerUserSystemParameters"])
        .spawn();
    Ok(())
}

#[cfg(not(windows))]
fn write_desktop(dir: &Path) -> Result<(), String> {
    let entry = desktop_entry_body(dir, &dir.join("icon.png"));
    let file = desktop_dir().join("rsedit.desktop");
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&file, entry).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&file, fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

#[cfg(not(windows))]
fn desktop_entry_body(dir: &Path, icon: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=rsedit\n\
         Comment=A minimal, fast, cross-platform video editor\n\
         Exec={}\n\
         Icon={}\n\
         Terminal=false\n\
         Categories=AudioVideo;Video;AudioVideoEditing;\n\
         Keywords=video;editor;cut;timeline;film;\n\
         StartupNotify=true\n",
        dir.join(exe_name()).display(),
        icon.display()
    )
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn remove_install(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
    }
    if let Ok(lnk) = start_menu_lnk() {
        let _ = fs::remove_file(lnk);
    }
    if let Ok(lnk) = desktop_lnk() {
        let _ = fs::remove_file(lnk);
    }
    let hkcu = winreg::enums::HKEY_CURRENT_USER;
    if let Ok(uninstall) = hkcu
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall")
    {
        let _ = uninstall.delete_subkey_all("rsedit");
    }
    Ok(())
}

#[cfg(not(windows))]
fn remove_install(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
    }
    let _ = fs::remove_file(app_menu_file());
    let _ = fs::remove_file(desktop_shortcut_file());
    Ok(())
}

#[cfg(windows)]
fn register_uninstall(dir: &Path, current_exe: &Path) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let uninstall_key = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\rsedit";
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(uninstall_key)
        .map_err(|e| format!("registry: {e}"))?;
    key.set_value("DisplayName", &APP_NAME).map_err(|e| format!("registry: {e}"))?;
    key.set_value("Publisher", &OWNER).map_err(|e| format!("registry: {e}"))?;
    key.set_value("InstallLocation", &dir.display().to_string()).map_err(|e| format!("registry: {e}"))?;
    let exe = current_exe.to_string_lossy().to_string();
    key.set_value("UninstallString", &format!("{exe} --uninstall"))
        .map_err(|e| format!("registry: {e}"))?;
    key.set_value("QuietUninstallString", &format!("{exe} --uninstall --silent"))
        .map_err(|e| format!("registry: {e}"))?;
    key.set_value("NoModify", &1u32).map_err(|e| format!("registry: {e}"))?;
    key.set_value("NoRepair", &1u32).map_err(|e| format!("registry: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared state + worker threads
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Phase {
    Idle,
    Querying,
    Downloading { got: u64, total: u64, mbps: f64 },
    Extracting,
    Configuring,
    Done { version: String },
    Removing,
    Removed,
    Error { msg: String },
}

impl Default for Phase {
    fn default() -> Self {
        Phase::Idle
    }
}

#[derive(Clone)]
struct Options {
    dir: PathBuf,
    start_menu: bool,
    desktop: bool,
}

#[derive(Clone, Default)]
struct Shared {
    phase: Arc<Mutex<Phase>>,
    log: Arc<Mutex<Vec<String>>>,
    cancel: Arc<AtomicBool>,
}

fn log_line(sh: &Shared, line: impl std::fmt::Display) {
    if let Ok(mut l) = sh.log.lock() {
        l.push(line.to_string());
        if l.len() > 400 {
            let excess = l.len() - 400;
            l.drain(..excess);
        }
    }
}

fn set_phase(sh: &Shared, p: Phase) {
    if let Ok(mut g) = sh.phase.lock() {
        *g = p;
    }
}

fn run_install(sh: Shared, opts: Options) {
    log_line(&sh, "Fetching latest release from GitHub…");
    set_phase(&sh, Phase::Querying);

    let release = match fetch_latest_release() {
        Ok(r) => r,
        Err(e) => {
            log_line(&sh, format!("Error: {e}"));
            set_phase(&sh, Phase::Error { msg: e });
            return;
        }
    };
    log_line(&sh, format!("Latest release: {}", release.tag_name));

    let Some(asset) = release.assets.iter().find(|a| asset_pick(&a.name)) else {
        let msg = "no portable build found for this platform in the latest release".into();
        log_line(&sh, format!("Error: {msg}"));
        set_phase(&sh, Phase::Error { msg });
        return;
    };
    log_line(&sh, format!("Found {} ({} MiB)", asset.name, asset.size / (1024 * 1024)));

    let tmp = env::temp_dir().join(format!("rsedit-{}.inst", release.tag_name));
    let _ = fs::remove_file(&tmp);

    log_line(&sh, "Downloading portable build…");
    if let Err(e) = download_to(&asset.browser_download_url, &tmp, sh.clone()) {
        let _ = fs::remove_file(&tmp);
        if e == "cancelled" {
            log_line(&sh, "Install cancelled.");
            set_phase(&sh, Phase::Idle);
        } else {
            log_line(&sh, format!("Error: {e}"));
            set_phase(&sh, Phase::Error { msg: e });
        }
        return;
    }
    if sh.cancel.load(Ordering::SeqCst) {
        let _ = fs::remove_file(&tmp);
        log_line(&sh, "Install cancelled.");
        set_phase(&sh, Phase::Idle);
        return;
    }

    set_phase(&sh, Phase::Extracting);
    log_line(&sh, format!("Extracting to {}…", opts.dir.display()));
    if let Err(e) = extract_archive(&tmp, &opts.dir) {
        let _ = fs::remove_file(&tmp);
        log_line(&sh, format!("Error: {e}"));
        set_phase(&sh, Phase::Error { msg: e });
        return;
    }
    let _ = fs::remove_file(&tmp);

    let exe = opts.dir.join(exe_name());
    if !exe.exists() {
        let msg = format!("installed program missing: {}", exe.display());
        log_line(&sh, format!("Error: {msg}"));
        set_phase(&sh, Phase::Error { msg });
        return;
    }

    set_phase(&sh, Phase::Configuring);
    if opts.start_menu {
        if let Err(e) = write_start_menu(&opts.dir) {
            log_line(&sh, format!("Warning: start menu shortcut: {e}"));
        } else {
            log_line(&sh, "Start menu shortcut created.");
        }
    }
    if opts.desktop {
        if let Err(e) = write_desktop(&opts.dir) {
            log_line(&sh, format!("Warning: desktop shortcut: {e}"));
        } else {
            log_line(&sh, "Desktop shortcut created.");
        }
    }
    #[cfg(windows)]
    {
        if let Some(exe) = env::current_exe().ok() {
            if let Err(e) = register_uninstall(&opts.dir, &exe) {
                log_line(&sh, format!("Warning: uninstall registry: {e}"));
            } else {
                log_line(&sh, "Uninstall entry registered.");
            }
        }
    }

    log_line(&sh, "Installed.");
    set_phase(&sh, Phase::Done {
        version: release.tag_name,
    });
}

fn run_uninstall(sh: Shared, dir: PathBuf) {
    set_phase(&sh, Phase::Removing);
    log_line(&sh, format!("Removing {}…", dir.display()));
    match remove_install(&dir) {
        Ok(()) => {
            log_line(&sh, "rsedit removed.");
            set_phase(&sh, Phase::Removed);
        }
        Err(e) => {
            log_line(&sh, format!("Error: {e}"));
            set_phase(&sh, Phase::Error { msg: e });
        }
    }
}

// ---------------------------------------------------------------------------
// GUI
// ---------------------------------------------------------------------------

struct InstallerApp {
    short: Shared,
    opts: Options,
    latest: Arc<Mutex<Option<String>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl InstallerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let dir = default_dir();
        let latest = Arc::new(Mutex::new(None));
        {
            let latest = latest.clone();
            let _ = std::thread::spawn(move || {
                let v = fetch_latest_release().ok().map(|r| r.tag_name);
                if let Ok(mut g) = latest.lock() {
                    *g = v;
                }
            });
        }
        Self {
            short: Shared::default(),
            opts: Options {
                dir,
                start_menu: true,
                desktop: true,
            },
            latest,
            worker: None,
        }
    }

    fn phase(&self) -> Phase {
        self.short
            .phase
            .lock()
            .map(|g| g.clone())
            .unwrap_or(Phase::Idle)
    }

    fn is_busy(&self) -> bool {
        matches!(
            self.phase(),
            Phase::Querying
                | Phase::Downloading { .. }
                | Phase::Extracting
                | Phase::Configuring
                | Phase::Removing
        )
    }

    fn kick_off(&mut self, uninstall: bool, silent: bool) {
        let sh = self.short.clone();
        let dir = self.opts.dir.clone();
        let opts = self.opts.clone();
        self.short.cancel.store(false, Ordering::SeqCst);
        let handle = std::thread::spawn(move || {
            if uninstall {
                run_uninstall(sh.clone(), dir);
            } else {
                run_install(sh.clone(), opts);
            }
            let _ = silent;
        });
        self.worker = Some(handle);
    }
}

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = BG;
    style.visuals.window_stroke = egui::Stroke::new(1.0, CARD_2);
    style.visuals.faint_bg_color = CARD;
    style.visuals.extreme_bg_color = CARD_2;
    style.visuals.selection.bg_fill = ACCENT_DARK;
    style.visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.bg_fill = ACCENT_DARK;
    style.visuals.hyperlink_color = ACCENT;
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    ctx.set_style(style);
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Reap finished worker threads.
        if let Some(h) = &self.worker {
            if h.is_finished() {
                self.worker = None;
            }
        }

        let busy = self.is_busy();
        if busy {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(300));
        }

        // ── Brand bar ────────────────────────────────────────────────────
        egui::TopBottomPanel::top("brand")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(15, 15, 18))
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(
                        egui::vec2(14.0, 14.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().circle_filled(dot.center(), 7.0, ACCENT);
                    ui.label(
                        egui::RichText::new("rsedit")
                            .strong()
                            .size(22.0)
                            .color(WHITE),
                    );
                    ui.label(egui::RichText::new("Installer").size(13.0).color(GRAY));
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let latest = self.latest.lock().unwrap().clone();
                            let text = match latest {
                                Some(v) => format!("Latest {v}"),
                                None => "Checking for updates…".into(),
                            };
                            ui.label(
                                egui::RichText::new(text).size(11.0).color(GRAY),
                            );
                        },
                    );
                });
                ui.add_space(6.0);
                let bar = ui.available_rect_before_wrap();
                let y = bar.max.y - 2.0;
                ui.painter()
                    .rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(bar.min.x, y),
                            egui::pos2(bar.max.x, bar.max.y),
                        ),
                        1.0,
                        ACCENT,
                    );
            });

        // ── Actions bar ──────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("actions")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(20, 12)))
            .show(ctx, |ui| {
                let installed = self
                    .opts
                    .dir
                    .join(exe_name())
                    .exists();
                ui.horizontal(|ui| {
                    if busy {
                        if ui
                            .add(
                                egui::Button::new("Cancel")
                                    .corner_radius(6.0)
                                    .fill(CARD_2),
                            )
                            .clicked()
                        {
                            self.short.cancel.store(true, Ordering::SeqCst);
                        }
                    } else {
                        let install_label = if installed {
                            "Install / Update"
                        } else {
                            "Install"
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(install_label).strong().color(egui::Color32::WHITE),
                                )
                                .corner_radius(6.0)
                                .fill(if installed { ACCENT_DARK } else { ACCENT })
                                .min_size(egui::vec2(120.0, 34.0)),
                            )
                            .clicked()
                        {
                            self.kick_off(false, false);
                        }
                        if installed {
                            if ui
                                .add(
                                    egui::Button::new("Run")
                                        .corner_radius(6.0)
                                        .fill(CARD_2)
                                        .min_size(egui::vec2(70.0, 34.0)),
                                )
                                .clicked()
                            {
                                let _ = Command::new(self.opts.dir.join(exe_name())).spawn();
                            }
                            if ui
                                .add(
                                    egui::Button::new("Open folder")
                                        .corner_radius(6.0)
                                        .fill(CARD_2),
                                )
                                .clicked()
                            {
                                #[cfg(windows)]
                                let _ = Command::new("explorer.exe")
                                    .arg(&self.opts.dir)
                                    .spawn();
                                #[cfg(not(windows))]
                                let _ = Command::new("xdg-open")
                                    .arg(&self.opts.dir)
                                    .spawn();
                            }
                            if ui
                                .add(
                                    egui::Button::new("Uninstall")
                                        .corner_radius(6.0)
                                        .fill(egui::Color32::from_rgb(60, 28, 28)),
                                )
                                .clicked()
                            {
                                self.kick_off(true, false);
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ver = installed_version(&self.opts.dir);
                        let text = match ver {
                            Some(v) => format!("Installed: v{}", v.trim_start_matches('v')),
                            None => "Not installed".into(),
                        };
                        ui.label(egui::RichText::new(text).size(11.0).color(DIM));
                    });
                });
            });

        // ── Body ─────────────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(20, 14)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.status_card(ui);
                        ui.add_space(12.0);
                        self.options_card(ui);
                        ui.add_space(12.0);
                        self.log_card(ui);
                    });
            });
    }
}

impl InstallerApp {
    fn status_card(&mut self, ui: &mut egui::Ui) {
        let phase = self.phase();
        egui::Frame::new()
            .fill(CARD)
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    self.status_icon(ui, &phase);
                    ui.vertical(|ui| {
                        let (title, sub): (String, String) = match &phase {
                            Phase::Idle => (
                                "Ready to install".into(),
                                format!(
                                    "{APP_NAME} will be downloaded from the latest \
                                     official release and installed for this user only."
                                ),
                            ),
                            Phase::Querying => {
                                ("Checking for updates…".into(), "Contacting GitHub…".into())
                            }
                            Phase::Downloading { got, total, mbps } => {
                                let pct = if *total > 0 {
                                    (*got as f32 / *total as f32 * 100.0).min(100.0)
                                } else {
                                    0.0
                                };
                                (
                                    format!("Downloading… {pct:.0}%"),
                                    format!(
                                        "{:.1} / {:.1} MiB  ({:.1} MiB/s)",
                                        *got as f64 / (1024.0 * 1024.0),
                                        *total as f64 / (1024.0 * 1024.0),
                                        mbps
                                    ),
                                )
                            }
                            Phase::Extracting => {
                                ("Installing…".into(), "Extracting files…".into())
                            }
                            Phase::Configuring => (
                                "Almost done…".into(),
                                "Creating start menu and desktop shortcuts…".into(),
                            ),
                            Phase::Done { version } => (
                                format!("Installed {}", version),
                                format!(
                                    "rsedit is ready to use. Launch it from {}.",
                                    self.opts.dir.display()
                                ),
                            ),
                            Phase::Removing => ("Uninstalling…".into(), "Removing files…".into()),
                            Phase::Removed => {
                                ("Uninstalled".into(), "rsedit has been removed.".into())
                            }
                            Phase::Error { msg } => {
                                ("Something went wrong".into(), msg.clone())
                            }
                        };
                        ui.label(egui::RichText::new(title).strong().size(16.0).color(WHITE));
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(sub).size(12.0).color(GRAY));
                    });
                });

                if let Phase::Downloading { got, total, .. } = &phase {
                    ui.add_space(8.0);
                    let frac = if *total > 0 {
                        (*got as f32 / *total as f32).min(1.0)
                    } else {
                        0.0
                    };
                    let bar = egui::ProgressBar::new(frac)
                        .desired_width(f32::INFINITY)
                        .fill(ACCENT)
                        .text("");
                    ui.add(bar);
                }

                if let Phase::Error { msg } = &phase {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(msg)
                            .size(11.0)
                            .color(RED)
                            .strong(),
                    );
                }
            });
    }

    fn status_icon(&self, ui: &mut egui::Ui, phase: &Phase) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::hover());
        let c = rect.center();
        let painter = ui.painter();
        match phase {
            Phase::Done { .. } | Phase::Removed => {
                painter.circle_filled(c, 17.0, egui::Color32::from_rgb(22, 48, 34));
                painter.circle_stroke(c, 17.0, egui::Stroke::new(1.5, GREEN));
                painter.text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    "\u{2713}",
                    egui::FontId::proportional(18.0),
                    GREEN,
                );
            }
            Phase::Error { .. } => {
                painter.circle_filled(c, 17.0, egui::Color32::from_rgb(58, 24, 24));
                painter.circle_stroke(c, 17.0, egui::Stroke::new(1.5, RED));
                painter.text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    "\u{0021}",
                    egui::FontId::proportional(18.0),
                    RED,
                );
            }
            Phase::Idle => {
                painter.circle_stroke(c, 13.0, egui::Stroke::new(2.0, ACCENT));
                painter.circle_filled(c, 4.0, ACCENT);
            }
_ => {
                // busy spinner
                painter.circle_filled(c, 17.0, CARD_2);
                let t = ui.input(|i| i.time) as f32;
                for k in 0..10u32 {
                    let a = t * 5.0 + k as f32 * std::f32::consts::TAU / 10.0;
                    let col = if k % 5 < 2 {
                        ACCENT
                    } else {
                        egui::Color32::from_rgb(48, 52, 60)
                    };
                    let d = egui::Vec2::angled(a);
                    painter.circle_filled(c + d * 11.0, 2.4, col);
                }
            }
        }
    }

    fn options_card(&mut self, ui: &mut egui::Ui) {
        let busy = self.is_busy();
        egui::Frame::new()
            .fill(CARD)
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Install options")
                        .strong()
                        .size(13.0)
                        .color(GRAY),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Location").size(12.0).color(GRAY));
                    let mut buf = self.opts.dir.display().to_string();
                    let resp =
                        ui.add(egui::TextEdit::singleline(&mut buf).desired_width(200.0));
                    if resp.changed() || resp.lost_focus() {
                        let t = buf.trim();
                        if !t.is_empty() {
                            self.opts.dir = PathBuf::from(t);
                        }
                    }
                    if ui.button("Browse…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            self.opts.dir = d;
                        }
                    }
                });
                ui.add_space(4.0);
                ui.add_enabled_ui(!busy, |ui| {
                    let menu_name = {
                        #[cfg(windows)]
                        {
                            "Add Start Menu shortcut"
                        }
                        #[cfg(not(windows))]
                        {
                            "Add applications menu shortcut"
                        }
                    };
                    ui.checkbox(&mut self.opts.start_menu, menu_name);
                    ui.checkbox(
                        &mut self.opts.desktop,
                        "Add desktop shortcut",
                    );
                });
            });
    }

    fn log_card(&mut self, ui: &mut egui::Ui) {
        let log = self.short.log.lock().unwrap().clone();
        egui::Frame::new()
            .fill(CARD)
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Details")
                        .strong()
                        .size(13.0)
                        .color(GRAY),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("installer_log")
                    .max_height(150.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 3.0);
                        if log.is_empty() {
                            ui.label(
                                egui::RichText::new("Nothing yet.")
                                    .size(11.0)
                                    .color(DIM),
                            );
                        }
                        for line in &log {
                            let color = if line.contains("Warning") {
                                egui::Color32::from_rgb(220, 180, 80)
                            } else if line.contains("Error") {
                                RED
                            } else {
                                GRAY
                            };
                            ui.label(
                                egui::RichText::new(line)
                                    .size(11.0)
                                    .monospace()
                                    .color(color),
                            );
                        }
                    });
            });
    }
}

// ---------------------------------------------------------------------------

fn main() -> Result<(), eframe::Error> {
    let args: Vec<String> = env::args().collect();

    // Headless uninstall (used by the Windows "Uninstall" / "Quiet Uninstall"
    // registry entries).
    if args.iter().any(|a| a == "--uninstall") {
        let dir = default_dir();
        match remove_install(&dir) {
            Ok(()) => {
                println!();
                println!("  rsedit has been removed.");
                println!();
            }
            Err(e) => {
                eprintln!("Uninstall error: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("rsedit Installer")
            .with_inner_size([620.0, 640.0])
            .with_min_inner_size([560.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "rsedit Installer",
        native_options,
        Box::new(|cc| Ok(Box::new(InstallerApp::new(cc)))),
    )
}