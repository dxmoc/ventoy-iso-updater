//! Windows ISOs: massgrave hand-off, Fido, UUP dump
//! (Python: detect_windows_builds .. _massgrave_handoff).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use regex::Regex;
use serde_json::Value;

use crate::Summary;
use crate::app::{Args, WinMethod};
use crate::catalog::{Item, Kind, WinSpec};
use crate::download::{fetch_tracked, move_with_progress};
use crate::manifest::{Manifest, entry_filename, record_version};
use crate::net::{Meta, http_get, verkey};

const FIDO_URL: &str = "https://raw.githubusercontent.com/pbatard/Fido/master/Fido.ps1";
const UUP_API: &str = "https://api.uupdump.net";
const UUP_GET: &str = "https://uupdump.net/get.php";

/// (key, label, spec) of a Windows item that still has to be fetched.
type Todo = (&'static str, &'static str, &'static WinSpec);

fn spec(it: &'static Item) -> Option<&'static WinSpec> {
    match &it.kind {
        Kind::Windows(p) => Some(p),
        _ => None,
    }
}

fn fill(tmpl: &str, build: &str) -> String {
    tmpl.replace("{}", build)
}

/// Latest build number per edition, keyed by the catalog `win` value, read from
/// the massgrave markdown. A failed lookup keeps its fallback -- the build only
/// shapes the target filename.
fn detect_windows_builds() -> HashMap<&'static str, String> {
    let mut builds: HashMap<&str, String> = [
        ("11", "26200.8655"),
        ("10", "19045.6456"),
        ("2022", "20348.5256"),
        ("2025", "26100.32995"),
    ]
    .into_iter()
    .map(|(k, v)| (k, v.to_string()))
    .collect();
    let md = |file: &str| {
        http_get(&format!(
            "https://raw.githubusercontent.com/massgravel/massgrave.dev/main/docs/{file}"
        ))
        .ok()
    };
    let grab = |md: &str, re: &str| Regex::new(re).ok()?.captures(md).map(|c| c[1].to_string());
    if let Some(md) = md("windows_11_links.md")
        && let Some(b) = grab(
            &md,
            r"(?s)Consumer 25H2.{0,60}?Build - ([\d.]+)[^\n]*Latest",
        )
    {
        builds.insert("11", b);
    }
    if let Some(md) = md("windows_10_links.md")
        && let Some(b) = grab(&md, r"Build - (19045\.\d+)")
    {
        builds.insert("10", b);
    }
    if let Some(md) = md("windows-server-links.md") {
        for year in ["2025", "2022"] {
            if let Some(b) = grab(
                &md,
                &format!(r"(?s)Windows Server {year}.*?Build - ([\d.]+)"),
            ) {
                builds.insert(year, b);
            }
        }
    }
    builds
}

// ---- elevation and helpers ------------------------------------------------ //

#[cfg(windows)]
pub fn is_admin() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

#[cfg(not(windows))]
pub fn is_admin() -> bool {
    false
}

#[cfg(windows)]
fn shell_execute(verb: &str, file: &str, params: Option<&str>) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (verb, file, params) = (wide(verb), wide(file), params.map(wide));
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    rc as isize > 32
}

/// Relaunch elevated (UAC); true if the elevated copy took over.
#[cfg(windows)]
pub fn relaunch_as_admin() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // The update notice was already shown in this window. (Not added twice:
    // clap rejects a repeated flag where argparse did not.)
    if !args.iter().any(|a| a == "--no-update-check") {
        args.push("--no-update-check".into());
    }
    let params: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
    shell_execute("runas", &exe.to_string_lossy(), Some(&params.join(" ")))
}

#[cfg(not(windows))]
pub fn relaunch_as_admin() -> bool {
    false
}

/// shutil.which: the first PATH entry holding `name` (with PATHEXT on Windows).
fn which(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE".into())
            .split(';')
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|d| exts.iter().map(move |e| d.join(format!("{name}{e}"))))
        .find(|p| p.is_file())
}

/// Run `cmd` to completion; kill it after `secs` seconds.
fn run_timeout(cmd: &mut Command, secs: u64) -> Result<()> {
    let mut child = cmd.spawn()?;
    let end = Instant::now() + Duration::from_secs(secs);
    while child.try_wait()?.is_none() {
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {secs} seconds");
        }
        thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Percent-encode a query value (only unreserved characters stay as they are).
fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Last path segment of a URL (Python: basename(urlparse(url).path)).
fn url_basename(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split(['?', '#']).next().unwrap_or("");
    path.find('/')
        .map_or("", |i| path[i..].rsplit('/').next().unwrap_or(""))
}

/// Open a page the way Python's webbrowser.open would; false if nothing could.
fn open_browser(url: &str) -> bool {
    #[cfg(windows)]
    {
        shell_execute("open", url, None)
    }
    #[cfg(not(windows))]
    {
        let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::env::var_os("WSL_INTEROP").is_some()
            || fs::read_to_string("/proc/version")
                .is_ok_and(|v| v.to_lowercase().contains("microsoft"));
        let tools: &[&str] = if cfg!(target_os = "macos") {
            &["open"]
        } else if wsl {
            &["wslview", "explorer.exe"]
        } else {
            &["xdg-open"]
        };
        tools.iter().any(|t| {
            Command::new(t)
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok()
        })
    }
}

// ---- Fido (official Microsoft ISOs), with massgrave fallback --------------- //

/// Download the newest Fido.ps1 into the temp dir.
fn ensure_fido() -> Result<PathBuf> {
    let path = std::env::temp_dir().join("ventoy_fido.ps1");
    fs::write(&path, http_get(FIDO_URL)?)?;
    Ok(path)
}

/// Ask Fido for the official Microsoft direct-download URL (or fail).
fn fido_url(ps: &Path, fido: &Path, win: &str, lang: &str) -> Result<String> {
    // Output goes through files: no pipe to drain while waiting with a timeout.
    let tmp = std::env::temp_dir();
    let (out_p, err_p) = (tmp.join("ventoy_fido.out"), tmp.join("ventoy_fido.err"));
    let run = run_timeout(
        Command::new(ps)
            .args(["-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(fido)
            .args([
                "-Win", win, "-Rel", "Latest", "-Lang", lang, "-Arch", "x64", "-GetUrl",
            ])
            .stdin(Stdio::null())
            .stdout(fs::File::create(&out_p)?)
            .stderr(fs::File::create(&err_p)?),
        180,
    );
    let read = |p: &Path| String::from_utf8_lossy(&fs::read(p).unwrap_or_default()).into_owned();
    let (out, err) = (read(&out_p), read(&err_p));
    let _ = (fs::remove_file(&out_p), fs::remove_file(&err_p));
    run?;
    let url = out
        .lines()
        .map(str::trim)
        .rfind(|l| l.to_lowercase().starts_with("http"));
    match url {
        Some(u) => Ok(u.to_string()),
        None => {
            let msg = out
                .lines()
                .find(|l| l.to_lowercase().contains("error"))
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .or(Some(err.trim()).filter(|e| !e.is_empty()))
                .unwrap_or("no URL");
            Err(anyhow!("{}", msg.chars().take(140).collect::<String>()))
        }
    }
}

/// The stored filename if `key` is already on the stick in release `rel`.
fn up_to_date(
    dest: &Path,
    manifest: &Manifest,
    args: &Args,
    key: &str,
    rel: &str,
) -> Option<String> {
    let stored = entry_filename(manifest, key)?;
    (!args.force && !stored.is_empty() && stored.contains(rel) && dest.join(stored).exists())
        .then(|| stored.to_string())
}

/// True if at least one selected Windows ISO is missing/outdated.
pub fn windows_needs_build(
    dest: &Path,
    manifest: &Manifest,
    args: &Args,
    items: &[&'static Item],
) -> bool {
    items
        .iter()
        .filter_map(|it| spec(it).map(|p| (it, p)))
        .any(|(it, p)| up_to_date(dest, manifest, args, it.key, p.rel).is_none())
}

/// Download the selected Windows ISOs via the chosen method (--windows):
/// massgrave hand-off (default), fido, or uup with a local build.
pub fn process_windows(
    dest: &Path,
    manifest: &mut Manifest,
    args: &Args,
    items: &[&'static Item],
    summary: &mut Summary,
) {
    let mut todo: Vec<Todo> = Vec::new();
    for it in items {
        let Some(p) = spec(it) else { continue };
        if let Some(stored) = up_to_date(dest, manifest, args, it.key, p.rel) {
            println!("==> {:<24} {}  [up to date]", it.label, stored);
            summary.push((stored, "SKIP (up to date)".into()));
            println!();
            continue;
        }
        todo.push((it.key, it.label, p));
    }
    if todo.is_empty() {
        return;
    }
    let method = args.windows.unwrap_or(WinMethod::Massgrave);
    if method == WinMethod::Massgrave {
        massgrave_handoff(dest, manifest, &todo, summary);
        return;
    }
    // Fido and UUP dump only cover consumer editions -- Server ISOs are
    // browser-only, so they stay on the massgrave hand-off regardless.
    let (server, rest): (Vec<Todo>, Vec<Todo>) = todo.into_iter().partition(|t| t.2.server);
    if !rest.is_empty() {
        if method == WinMethod::Fido {
            process_fido(dest, manifest, args, &rest, summary);
        } else {
            process_uup(dest, manifest, &rest, summary);
        }
    }
    if !server.is_empty() {
        massgrave_handoff(dest, manifest, &server, summary);
    }
}

// ---- UUP dump: official ISOs from Windows Update, built locally ----------- //

/// True if `pid` still refers to a running process (best-effort).
fn pid_alive(pid: &str) -> bool {
    let Ok(pid) = pid.parse::<u32>() else {
        return false;
    };
    if cfg!(windows) {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
    } else {
        // /proc where it exists (no `kill` binary in minimal Linux images), else kill -0.
        Path::new(&format!("/proc/{pid}")).exists()
            || Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    }
}

/// Newest *retail* amd64 build (uuid, build number) for Windows `win`/`rel`
/// (e.g. "11"/"25H2") from the UUP dump index.
fn uup_latest_uuid(win: &str, rel: &str) -> Result<(String, String)> {
    let q = pct(&format!("Windows {win} {rel} amd64"));
    let data: Value =
        serde_json::from_str(&http_get(&format!("{UUP_API}/listid.php?search={q}"))?)?;
    let items: Vec<&Value> = match &data["response"]["builds"] {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => o.values().collect(),
        _ => vec![],
    };
    let bad = ["insider", "dev", "beta", "canary", "preview"];
    let title = |b: &Value| b["title"].as_str().unwrap_or("").to_lowercase();
    let cand: Vec<&Value> = items
        .into_iter()
        .filter(|b| {
            let t = title(b);
            b["arch"] == "amd64"
                && t.contains(&rel.to_lowercase())
                && !bad.iter().any(|w| t.contains(w))
        })
        .collect();
    // rev(): max_by_key keeps the last maximum, Python's max() the first.
    let best = cand
        .iter()
        .rev()
        .max_by_key(|b| verkey(b["build"].as_str().unwrap_or("0")))
        .ok_or_else(|| anyhow!("no retail UUP build for Windows {win} {rel}"))?;
    let uuid = best["uuid"].as_str().ok_or_else(|| anyhow!("'uuid'"))?;
    Ok((
        uuid.to_string(),
        best["build"].as_str().unwrap_or("?").to_string(),
    ))
}

/// Fetch the UUP dump convert package, run it (downloads the UUP set from
/// Windows Update and assembles the ISO with DISM), then move the finished ISO
/// onto the stick. Returns (out_path, save_name, build). Needs admin.
fn uup_build_iso(
    win: &str,
    rel: &str,
    lang: &str,
    editions: &str,
    work_root: &Path,
    dest: &Path,
    tmpl: &str,
) -> Result<(PathBuf, String, String)> {
    let (uuid, build) = uup_latest_uuid(win, rel)?;
    let save_name = fill(tmpl, &build);
    println!(
        "    build {}  (uuid {})",
        build,
        uuid.chars().take(8).collect::<String>()
    );

    let url = format!(
        "{UUP_GET}?id={}&pack={}&edition={}",
        pct(&uuid),
        pct(lang),
        pct(editions)
    );
    let ua = format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) VentoyISOUpdater/{}",
        crate::VERSION
    );
    let pkg = ureq::post(&url)
        .header("User-Agent", &ua)
        .config()
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .send_form([("autodl", "2"), ("updates", "1"), ("cleanup", "1")])?
        .body_mut()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()?;

    // Build dir must be space-free (the UUP converter refuses spaces in paths).
    let bdir = work_root.join(format!("uup_{win}_{lang}_{build}"));
    let lock = bdir.join(".building.pid");
    if lock.exists() {
        // Another run (or a crashed one) owns this directory. Never clobber a
        // build in progress -- concurrent runs corrupt each other's downloads.
        let owner = fs::read_to_string(&lock)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "?".into());
        if pid_alive(&owner) {
            bail!(
                "another build (pid {owner}) is already using {}",
                bdir.display()
            );
        }
        println!("    (clearing stale build dir from pid {owner})");
    }
    // Reset the scripts/converter but keep an already-downloaded UUP set: it is
    // several GB and aria2 skips files it already has, making retries cheap.
    if bdir.is_dir() {
        let mut kept = 0;
        for entry in fs::read_dir(&bdir)?.flatten() {
            let p = entry.path();
            if entry.file_name() == "UUPs" {
                kept = fs::read_dir(&p).map(|d| d.count()).unwrap_or(0);
                continue;
            }
            let _ = if p.is_dir() {
                fs::remove_dir_all(&p)
            } else {
                fs::remove_file(&p)
            };
        }
        if kept > 0 {
            println!("    reusing {kept} already-downloaded UUP files");
        }
    }
    fs::create_dir_all(&bdir)?;
    fs::write(&lock, std::process::id().to_string())?;
    zip::ZipArchive::new(Cursor::new(pkg))?.extract(&bdir)?;

    // The bundled get_aria2.ps1 downloads aria2c.exe and verifies it with
    // Get-FileHash. If the fresh binary is still locked (antivirus scan) the
    // check fails, it re-downloads over the locked file and the run aborts at a
    // `pause`. Reuse a cached, known-good aria2c.exe and stub the check out.
    let fdir = bdir.join("files");
    fs::create_dir_all(&fdir)?;
    let cache = work_root.join("ventoy_aria2c.exe");
    if cache.exists() {
        // Best-effort: if the target is locked (a concurrent build has it open)
        // leave the bundled downloader in place rather than failing the ISO.
        if let Err(e) = fs::copy(&cache, fdir.join("aria2c.exe"))
            .and_then(|_| fs::write(fdir.join("get_aria2.ps1"), "Exit 0\n"))
        {
            println!("    (could not pre-place aria2c.exe: {e} -- using the bundled downloader)");
        }
    }

    // Make the converter exit cleanly instead of waiting on a keypress.
    let cfg = bdir.join("ConvertConfig.ini");
    let txt = fs::read_to_string(&cfg)?;
    let re = Regex::new(r"AutoExit\s*=\s*0")?;
    fs::write(&cfg, re.replace_all(&txt, "AutoExit     =1").as_bytes())?;

    println!("    downloading the UUP set and building the ISO (this takes a while)...");
    let log = bdir.join("build.log");
    let lf = fs::File::create(&log)?;
    let mut cmd = Command::new("cmd");
    cmd.arg("/c")
        .arg(bdir.join("uup_download_windows.cmd"))
        .current_dir(&bdir)
        .stdin(Stdio::null())
        .stdout(lf.try_clone()?)
        .stderr(lf);
    // The UUP scripts call their helpers by bare name (`call convert-UUP.cmd`).
    // NoDefaultCurrentDirectoryInExePath -- often set by corporate policy --
    // stops cmd.exe resolving those from the working directory, so the
    // conversion silently never runs. Drop it for the child.
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy()
            .eq_ignore_ascii_case("NoDefaultCurrentDirectoryInExePath")
        {
            cmd.env_remove(k);
        }
    }
    run_timeout(&mut cmd, 7200)?;

    // Keep a good aria2c.exe for the next build (see the stub above).
    let got_aria2 = fdir.join("aria2c.exe");
    if !cache.exists() && got_aria2.exists() {
        let _ = fs::copy(&got_aria2, &cache);
    }

    let src = fs::read_dir(&bdir)?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .to_lowercase()
                .ends_with(".iso")
        })
        .map(|e| e.path())
        .max_by_key(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .ok_or_else(|| anyhow!("UUP produced no ISO (see {})", log.display()))?;
    let out = dest.join(&save_name);
    if out.exists() {
        fs::remove_file(&out)?;
    }
    move_with_progress(&src, &out)?; // scratch dir -> stick, several GB
    let _ = fs::remove_dir_all(&bdir);
    Ok((out, save_name, build))
}

fn process_uup(dest: &Path, manifest: &mut Manifest, todo: &[Todo], summary: &mut Summary) {
    if cfg!(windows) && !is_admin() {
        println!("    UUP dump assembles the ISO with DISM and needs admin rights.");
        println!("    Re-run this script as administrator to build Windows ISOs.\n");
        for (_, _, p) in todo {
            summary.push((fill(p.tmpl, p.rel), "SKIP (needs admin for UUP)".into()));
        }
        return;
    }
    let work = std::env::temp_dir();
    for &(key, label, p) in todo {
        let lang = if p.lang == "German" { "de-de" } else { "en-us" };
        println!("==> {:<24} UUP dump build (Win {}, {})", label, p.win, lang);
        let built = uup_build_iso(p.win, p.rel, lang, "core;professional", &work, dest, p.tmpl)
            .and_then(|(out, name, build)| Ok((fs::metadata(&out)?.len(), name, build)));
        match built {
            Ok((size, name, build)) => {
                println!(
                    "    ok  {}  ({:.2} GB)\n",
                    name,
                    size as f64 / (1u64 << 30) as f64
                );
                let meta = Meta {
                    size: Some(size),
                    mtime: None,
                };
                record_version(
                    dest,
                    manifest,
                    key,
                    &format!("uup:{}/{}", p.win, build),
                    &name,
                    Some(&meta),
                );
                summary.push((name, "OK (built via UUP)".into()));
            }
            Err(e) => {
                println!("    UUP build failed: {e}\n");
                summary.push((fill(p.tmpl, p.rel), format!("FAIL: {e}")));
            }
        }
    }
}

fn process_fido(
    dest: &Path,
    manifest: &mut Manifest,
    args: &Args,
    todo: &[Todo],
    summary: &mut Summary,
) {
    let ps = which("powershell").or_else(|| which("pwsh"));
    let mut fallback: Vec<Todo> = Vec::new();
    let mut fido: Option<PathBuf> = None;
    for &t in todo {
        let (key, label, p) = t;
        let Some(ps) = &ps else {
            fallback.push(t);
            continue;
        };
        if fido.is_none() {
            match ensure_fido() {
                Ok(f) => fido = Some(f),
                Err(e) => {
                    println!("    could not fetch Fido ({e}) -> massgrave fallback");
                    fallback.push(t);
                    continue;
                }
            }
        }
        println!(
            "==> {:<24} querying Microsoft via Fido ({})...",
            label, p.lang
        );
        match fido_url(ps, fido.as_deref().unwrap_or(Path::new("")), p.win, p.lang) {
            Ok(url) => {
                let name = Some(url_basename(&url))
                    .filter(|n| !n.is_empty())
                    .map_or_else(|| fill(p.tmpl, p.rel), str::to_string);
                fetch_tracked(
                    dest,
                    manifest,
                    key,
                    label,
                    &url,
                    &name,
                    &[],
                    args,
                    summary,
                    None,
                );
            }
            Err(e) => {
                println!("    Fido failed ({e}) -> massgrave fallback\n");
                fallback.push(t);
            }
        }
    }
    if !fallback.is_empty() {
        massgrave_handoff(dest, manifest, &fallback, summary);
    }
}

// ---- massgrave hand-off: manual browser download, automatic rename -------- //

#[derive(Clone)]
struct Plan {
    key: &'static str,
    target: String,
    win: &'static str,
    lang: &'static str,
    page: &'static str,
    pick: &'static str,
}

fn iso_files(folders: &[PathBuf]) -> Vec<PathBuf> {
    folders
        .iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .to_lowercase()
                .ends_with(".iso")
        })
        .map(|e| e.path())
        .collect()
}

/// How well a downloaded filename matches an expected Windows ISO.
fn match_score(fname: &str, win: &str, lang: &str) -> u32 {
    let n = fname.to_lowercase().replace(['-', ' '], "_");
    // Consumer names read "win_11"/"windows_10"; Server names "windows_server_2025".
    // (\D|$) stands in for Python's (?!\d), which the regex crate lacks.
    let re = Regex::new(&format!(
        r"(win(dows)?|server)_?{}(\D|$)",
        regex::escape(win)
    ))
    .expect("valid regex");
    if !re.is_match(&n) {
        return 0; // wrong Windows version
    }
    let words: &[&str] = match lang {
        "German" => &["german", "deutsch", "de_de"],
        "English" => &["english", "en_us"],
        _ => &[],
    };
    if words.iter().any(|w| n.contains(w)) {
        4
    } else {
        2
    }
}

const WATCH_POLL: Duration = Duration::from_secs(2); // between folder scans

/// Set while the download watch runs: Ctrl+C then only ends the watch
/// (app's handler sets STOP_WATCH) instead of the whole program.
pub static WATCHING: AtomicBool = AtomicBool::new(false);
pub static STOP_WATCH: AtomicBool = AtomicBool::new(false);
const WATCH_STABLE: Duration = Duration::from_secs(3); // size held this long = finished
const WATCH_HEARTBEAT: Duration = Duration::from_secs(60); // between "still waiting" notes

/// Watch the stick and the Downloads folder for the manually downloaded
/// Windows ISOs, then move/rename each to its canonical name.
///
/// A finished file is recognized by its size holding steady for WATCH_STABLE,
/// measured as a timestamp, not as "unchanged between two polls": moving an
/// ISO onto the stick blocks this loop for minutes, and a poll-to-poll rule
/// would make everything that finished meanwhile wait out another cycle.
fn windows_watch_rename(
    dest: &Path,
    manifest: &mut Manifest,
    plan: Vec<Plan>,
    summary: &mut Summary,
    wait_min: u64,
) {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
    let downloads = PathBuf::from(home).join("Downloads");
    let folders = [dest.to_path_buf(), downloads.clone()];
    let mut known: HashSet<PathBuf> = iso_files(&folders).into_iter().collect(); // ignore what's already there
    let mut pending = plan;
    let mut seen: HashMap<PathBuf, (u64, Instant)> = HashMap::new(); // path -> (size, when first seen)

    println!("Watching for your downloads:");
    println!("  {}", dest.display());
    if downloads.is_dir() {
        println!("  {}", downloads.display());
    }
    println!("Each ISO is renamed and moved onto the stick automatically.");
    println!("Copying one onto the stick takes a few minutes and pauses the");
    println!("watch -- anything you download meanwhile is picked up right after.");
    println!(
        "Press Ctrl+C when you're done / want to skip the rest. DONT PANIC IF IT LOOKS STUCK!\n"
    );

    let started = Instant::now();
    let deadline = started + Duration::from_secs(wait_min * 60);
    let mut next_beat = started + WATCH_HEARTBEAT;
    STOP_WATCH.store(false, Ordering::SeqCst);
    WATCHING.store(true, Ordering::SeqCst);
    while !pending.is_empty() && Instant::now() < deadline {
        if STOP_WATCH.load(Ordering::SeqCst) {
            println!("\n  (stopped watching)");
            break;
        }
        for path in iso_files(&folders) {
            if pending.is_empty() {
                break;
            }
            if known.contains(&path) {
                continue;
            }
            let Ok(size) = fs::metadata(&path).map(|m| m.len()) else {
                continue;
            };
            if size < 1 << 30 {
                continue; // too small to be a Windows ISO
            }
            let now = Instant::now();
            match seen.get(&path) {
                Some(&(s, since)) if s == size => {
                    if now - since < WATCH_STABLE {
                        continue; // settled only just now
                    }
                }
                _ => {
                    seen.insert(path, (size, now)); // still growing -> restart the clock
                    continue;
                }
            }
            let base = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut scored: Vec<(u32, usize)> = pending
                .iter()
                .enumerate()
                .map(|(i, e)| (match_score(&base, e.win, e.lang), i))
                .collect();
            scored.sort_by_key(|s| std::cmp::Reverse(s.0)); // stable, like sorted(reverse=True)
            let (best_score, mut best) = scored[0];
            if best_score < 4 {
                // Not every massgrave filename spells out the language. Take it
                // anyway when exactly one pending ISO is left for that Windows
                // version; otherwise keep waiting rather than guess wrong.
                let same: Vec<usize> = scored.iter().filter(|s| s.0 >= 2).map(|s| s.1).collect();
                if best_score < 2 || same.len() != 1 {
                    continue;
                }
                best = same[0];
            }
            let e = pending[best].clone();
            let out = dest.join(&e.target);
            println!("  {:<28} -> {}", base, e.target);
            let moved = (|| -> Result<u64> {
                if path != out {
                    move_with_progress(&path, &out)?;
                }
                Ok(fs::metadata(&out)?.len())
            })();
            match moved {
                Ok(size) => {
                    let meta = Meta {
                        size: Some(size),
                        mtime: None,
                    };
                    record_version(
                        dest,
                        manifest,
                        e.key,
                        "massgrave (manual)",
                        &e.target,
                        Some(&meta),
                    );
                    summary.push((e.target.clone(), "OK (manual, renamed)".into()));
                    pending.remove(best);
                    known.insert(path.clone());
                    known.insert(out); // don't re-scan what we just wrote
                    seen.remove(&path);
                    next_beat = Instant::now() + WATCH_HEARTBEAT;
                    if !pending.is_empty() {
                        println!("  watching again -- {} ISO(s) to go\n", pending.len());
                    }
                }
                Err(err) => {
                    println!("  could not move {base}: {err}");
                    known.insert(path);
                }
            }
        }
        if pending.is_empty() {
            break;
        }
        let now = Instant::now();
        if now >= next_beat {
            println!(
                "  still waiting for {} ISO(s) -- {} min elapsed",
                pending.len(),
                (now - started).as_secs() / 60
            );
            next_beat = now + WATCH_HEARTBEAT;
        }
        thread::sleep(WATCH_POLL);
    }
    WATCHING.store(false, Ordering::SeqCst);

    for e in pending {
        summary.push((e.target, "MANUAL (still missing)".into()));
    }
}

/// Open the massgrave pages, then watch for and rename what gets saved.
fn massgrave_handoff(dest: &Path, manifest: &mut Manifest, items: &[Todo], summary: &mut Summary) {
    let builds = detect_windows_builds();
    let plan: Vec<Plan> = items
        .iter()
        .map(|&(key, _label, p)| Plan {
            key,
            target: fill(p.tmpl, builds.get(p.win).map_or("unknown", String::as_str)),
            win: p.win,
            lang: p.lang,
            page: p.page,
            pick: p.pick,
        })
        .collect();

    println!("\n{0} WINDOWS ISOs (massgrave.dev) {0}", "=".repeat(16));
    println!("massgrave's Windows ISOs are browser-only (rotating mirror links),");
    println!("so these need a manual click. Opening the pages now -- pick:\n");
    for e in &plan {
        println!("  {:<48} -> {}", e.pick, e.target);
    }
    println!("\nSave them anywhere (the stick or your Downloads folder);");
    println!("the script picks them up and renames them for you.\n");

    let mut opened = HashSet::new();
    for e in &plan {
        if opened.insert(e.page) && !open_browser(e.page) {
            println!("  (open manually: {})", e.page);
        }
    }
    windows_watch_rename(dest, manifest, plan, summary, 90);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_score_versions_and_languages() {
        let en11 = "en-us_windows_11_consumer_editions_version_25h2_x64_dvd.iso";
        assert_eq!(match_score(en11, "11", "English"), 4);
        assert_eq!(match_score(en11, "11", "German"), 2);
        assert_eq!(match_score(en11, "10", "English"), 0);
        assert_eq!(
            match_score("Win10_22H2_German_x64v1.iso", "10", "German"),
            4
        );
        assert_eq!(match_score("Win 11 25H2 Deutsch.iso", "11", "German"), 4);
        assert_eq!(match_score("Win_110.iso", "11", "English"), 0);
        let srv = "en-us_windows_server_2025_updated_x64_dvd.iso";
        assert_eq!(match_score(srv, "2025", "English"), 4);
        assert_eq!(match_score(srv, "2022", "English"), 0);
    }

    #[test]
    fn helpers() {
        assert_eq!(
            url_basename("https://h.example/a/b/Win11_25H2_German_x64.iso?t=1&e=2"),
            "Win11_25H2_German_x64.iso"
        );
        assert_eq!(url_basename("https://h.example"), "");
        assert_eq!(url_basename("https://h.example/dir/"), "");
        assert_eq!(pct("Windows 11 25H2 amd64"), "Windows%2011%2025H2%20amd64");
        assert_eq!(pct("core;professional"), "core%3Bprofessional");
        assert_eq!(
            fill("Win_11_25H2_German_{}.iso", "26200.1"),
            "Win_11_25H2_German_26200.1.iso"
        );
        assert!(!pid_alive("not-a-pid"));
        assert!(pid_alive(&std::process::id().to_string()));
    }

    #[test]
    fn iso_files_lists_only_isos() {
        let dir = std::env::temp_dir().join(format!("ventoy_win_iso_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for f in ["a.ISO", "b.iso", "c.txt"] {
            fs::write(dir.join(f), "").unwrap();
        }
        let mut got: Vec<String> = iso_files(&[dir.clone(), dir.join("missing")])
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        got.sort();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(got, ["a.ISO", "b.iso"]);
    }
}
