//! CLI and the top-level run (Python: main()).

use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use clap::{Parser, ValueEnum};

use crate::catalog::{Item, Kind, Resolved};
use crate::manifest::{Check, Manifest};
use crate::style::{self, bold, dim};
use crate::{Summary, autoinstall, cleanup, download, manifest, net, ui, windows};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Preset {
    Standard,
    Advanced,
    Everything,
    /// the best mix that fits the target drive
    Fit,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Cleanup {
    Ask,
    Yes,
    No,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum WinMethod {
    Massgrave,
    Uup,
    Fido,
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "ventoy-iso-updater",
    version,
    about = "Update Ventoy ISOs to newest."
)]
pub struct Args {
    /// target folder / Ventoy drive (if omitted, you get an interactive picker)
    #[arg(long)]
    pub dest: Option<String>,
    /// re-download every ISO, ignoring the version manifest
    #[arg(long)]
    pub force: bool,
    /// only report what would be downloaded, deleted or left alone; writes nothing
    #[arg(long)]
    pub dry_run: bool,
    /// skip the SHA-256 check against the checksum file published next to the image
    #[arg(long)]
    pub no_verify: bool,
    /// don't ask GitHub at startup whether a newer release is out
    #[arg(long)]
    pub no_update_check: bool,
    /// skip the fastest-mirror speed test; use the default source directly
    #[arg(long)]
    pub no_mirror_test: bool,
    /// skip the interactive menu and use this selection
    #[arg(long, value_enum)]
    pub preset: Option<Preset>,
    /// what to do with superseded ISOs: ask (default), yes (delete), no (only list)
    #[arg(long, value_enum, default_value = "ask")]
    pub cleanup: Cleanup,
    /// Windows ISO method, skipping the prompt (massgrave is the default)
    #[arg(long, value_enum)]
    pub windows: Option<WinMethod>,
}

/// Resolvers running at once. net keeps them to one request per host, so this
/// only bounds how many different hosts are talked to at the same time.
const RESOLVE_WORKERS: usize = 16;

const MEMTEST_URL: &str = "https://www.memtest86.com/downloads/memtest86-usb.zip";
const MEMTEST_IMG: &str = "memtest86-usb.img";

/// Whole program; returns the exit code.
pub fn run() -> i32 {
    // Parsing first, so --help/--version exit without the send-off line.
    let args = Args::parse();
    // Ctrl+C during the Windows download watch only ends the watch; anywhere
    // else it stops queued lookups and exits. Partial downloads stay as .part
    // and resume next run; copies, extracts and build locks are removed.
    let _ = ctrlc::set_handler(|| {
        if windows::WATCHING.load(Ordering::SeqCst) {
            windows::STOP_WATCH.store(true, Ordering::SeqCst);
        } else {
            net::abort();
            download::remove_temps();
            println!("\nAborted.");
            farewell();
            std::process::exit(1);
        }
    });
    let code = run_with(args);
    farewell();
    pause_if_own_console();
    code
}

/// Started by double-click, Windows gives the program its own console and
/// closes it on exit, taking the summary with it. Only then wait for Enter.
fn pause_if_own_console() {
    #[cfg(windows)]
    {
        let mut pids = [0u32; 2];
        let n = unsafe {
            windows_sys::Win32::System::Console::GetConsoleProcessList(pids.as_mut_ptr(), 2)
        };
        if n == 1 && std::io::stdin().is_terminal() {
            println!("Press Enter to close this window.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
    }
}

/// Parting line, italic only on a real terminal so piped output stays clean.
pub(crate) fn farewell() {
    let msg = "See you Space Cowboy!";
    if std::io::stdout().is_terminal() {
        println!("\n\x1b[3m{msg}\x1b[0m");
    } else {
        println!("\n{msg}");
    }
}

/// Shells expand a bare ~ before we see it, but cmd.exe and a quoted "~/isos"
/// don't -- without this that ~ becomes a folder literally named "~".
fn expand_dest(target: &str) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let p = match (target.strip_prefix('~'), home) {
        (Some(rest), Some(h)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            PathBuf::from(h).join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(target),
    };
    std::path::absolute(&p).unwrap_or(p)
}

fn run_with(mut args: Args) -> i32 {
    if !args.no_update_check {
        manifest::check_for_update();
    }
    let dest = match &args.dest {
        Some(d) => expand_dest(d),
        None => expand_dest(&ui::choose_dest().to_string_lossy()),
    };
    if !args.dry_run
        && let Err(e) = std::fs::create_dir_all(&dest)
    {
        eprintln!("Cannot create {}: {e}", dest.display());
        return 1;
    }
    let mut manifest = manifest::load(&dest);

    let mut selection = ui::choose_selection(args.preset, &dest, &manifest);
    // Windows is never part of a preset -- ask for it separately. That prompt
    // also settles the method, unless --windows already did.
    if args.preset.is_none() {
        let already: HashSet<&str> = selection.iter().map(|it| it.key).collect();
        let win = ui::choose_windows(&already, &mut args);
        selection.extend(win);
    }
    let method = *args.windows.get_or_insert(WinMethod::Massgrave);
    let iso_items: Vec<_> = selection
        .iter()
        .copied()
        .filter(|it| matches!(it.kind, Kind::Iso(_)))
        .collect();
    let win_items: Vec<_> = selection
        .iter()
        .copied()
        .filter(|it| matches!(it.kind, Kind::Windows(_)))
        .collect();
    let memtest = selection.iter().any(|it| matches!(it.kind, Kind::Memtest));
    if selection.is_empty() {
        println!("Nothing selected. Exiting.");
        return 0;
    }

    // UUP dump builds the ISO locally with DISM, which needs admin. Ask for
    // elevation once (a single UAC prompt) so the run can proceed unattended.
    if !win_items.is_empty()
        && method == WinMethod::Uup
        && !args.dry_run
        && cfg!(windows)
        && !windows::is_admin()
        && windows::windows_needs_build(&dest, &manifest, &args, &win_items)
    {
        println!("\nWindows ISOs (UUP dump) need administrator rights (DISM).");
        println!("Requesting elevation -- please confirm the UAC prompt...");
        if windows::relaunch_as_admin(&dest) {
            println!("Continuing in the elevated window.");
            return 0;
        }
        println!("Elevation refused -> Windows ISOs will be skipped this run.\n");
    }

    println!("\nTarget: {}", dest.display());
    println!(
        "{}",
        dim(&format!(
            "Version manifest: {}",
            dest.join(crate::MANIFEST_NAME).display()
        ))
    );
    println!(
        "Selected: {} ISOs{}{}\n",
        iso_items.len(),
        if memtest { " + MemTest86" } else { "" },
        if win_items.is_empty() {
            String::new()
        } else {
            format!(" + {} Windows", win_items.len())
        }
    );

    // ---- phase 1: detect newest versions, then show the plan ----
    let mut summary: Summary = Vec::new();
    let mut resolved: Vec<(String, String, Resolved)> = Vec::new();
    let mut checked: HashMap<&str, Check> = HashMap::new();
    let mut todo: Vec<(&str, String, String, u64)> = Vec::new(); // label, file, reason, bytes
    let mut failed: Vec<String> = Vec::new();
    let detected = detect_all(&iso_items, &dest, &manifest, args.force);
    style::print_notes();
    for (item, res) in iso_items.iter().zip(detected) {
        match res {
            Err(e) => {
                failed.push(format!("{}: DETECT FAILED ({e})", item.label));
                summary.push((item.label.to_string(), format!("FAIL (detect): {e}")));
            }
            Ok((r, check)) => {
                let (need, reason, size) = match &check {
                    Some(c) => (
                        c.need,
                        c.reason.clone(),
                        c.meta.as_ref().and_then(|m| m.size),
                    ),
                    None => (true, "forced".to_string(), None),
                };
                if need {
                    let bytes = size.unwrap_or(u64::from(item.size_mb) * 1_000_000);
                    todo.push((item.label, r.name.clone(), reason, bytes));
                }
                if let Some(c) = check {
                    checked.insert(item.key, c);
                }
                resolved.push((item.key.to_string(), item.label.to_string(), r));
            }
        }
    }
    let memtest_check = memtest.then(|| memtest_check(&dest, &manifest, &args));
    if let Some(c) = memtest_check.as_ref().filter(|c| c.need) {
        let size = crate::catalog::find("memtest").map_or(0, |it| it.size_mb);
        todo.push((
            "MemTest86",
            MEMTEST_IMG.into(),
            c.reason.clone(),
            u64::from(size) * 1_000_000,
        ));
    }
    print_plan(&todo, resolved.len() + memtest as usize, &failed);
    if !win_items.is_empty() {
        let how = match method {
            WinMethod::Uup => "UUP dump (official, local build)",
            WinMethod::Fido => "Fido (official MS link)",
            WinMethod::Massgrave => "massgrave hand-off",
        };
        // Every Windows method drives a browser or builds an image locally, so
        // a dry run has nothing to report without doing the work.
        let skipped = if args.dry_run {
            ", skipped in --dry-run"
        } else {
            ""
        };
        println!("Windows: {} ISO(s) via {how}{skipped}", win_items.len());
    }

    // ---- phase 2: sequential downloads (only what changed) ----
    let (mut n, total) = (0, todo.len());
    let mut header = |label: &str, name: &str| {
        n += 1;
        println!("\n[{n}/{total}] {}  {}", bold(label), dim(name));
    };
    for (key, label, r) in &resolved {
        let check = checked.remove(key.as_str());
        if !args.dry_run && check.as_ref().is_none_or(|c| c.need) {
            header(label, &r.name);
        }
        download::fetch_tracked(
            &dest,
            &mut manifest,
            key,
            label,
            &r.url,
            &r.name,
            &r.mirrors,
            &args,
            &mut summary,
            check,
        );
    }
    if let Some(check) = memtest_check {
        if !args.dry_run && check.need {
            header("MemTest86", MEMTEST_IMG);
        }
        update_memtest(&dest, &mut manifest, &args, &mut summary, check);
    }
    if !win_items.is_empty() && args.dry_run {
        for it in &win_items {
            summary.push((
                it.label.into(),
                "SKIP (Windows is not run in --dry-run)".into(),
            ));
        }
    } else if !win_items.is_empty() {
        println!();
        windows::process_windows(&dest, &mut manifest, &args, &win_items, &mut summary);
    }

    // Last, so the manifest already holds this run's filenames -- what is left
    // over can only be previous versions. A dry run lists but never deletes.
    let extra: &[(&str, &str, &str)] = if memtest {
        &[("memtest", "MemTest86", MEMTEST_IMG)]
    } else {
        &[]
    };
    let current = cleanup::current_files(&manifest, &resolved, extra);
    let mode = if args.dry_run {
        Cleanup::No
    } else {
        args.cleanup
    };
    cleanup::cleanup_superseded(&dest, &mut manifest, &current, mode, &mut summary);

    // After the cleanup, so templates are wired to the files that survived.
    autoinstall::sync_auto_install(&dest, &manifest, &args);

    print_summary(&summary, &dest);
    0
}

/// Resolve every item on RESOLVE_WORKERS threads; results in catalog order.
/// The update check rides along, since it is one more request per item.
#[allow(clippy::type_complexity)]
fn detect_all(
    items: &[&'static Item],
    dest: &Path,
    manifest: &Manifest,
    force: bool,
) -> Vec<anyhow::Result<(Resolved, Option<Check>)>> {
    let next = AtomicUsize::new(0);
    // One live line on a terminal; elsewhere nothing until the plan.
    let tty = std::io::stdout().is_terminal();
    let done = Mutex::new(0);
    let tick = || {
        let mut d = done.lock().unwrap();
        *d += 1;
        if tty {
            print!("\rChecking versions {}/{}", *d, items.len());
            let _ = std::io::stdout().flush();
        }
    };
    let slots: Vec<Mutex<Option<_>>> = items.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..RESOLVE_WORKERS.min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let Kind::Iso(resolve) = item.kind else {
                        unreachable!()
                    };
                    let res = resolve().map(|r| {
                        let check = (!force).then(|| {
                            manifest::check_update(dest, manifest.get(item.key), &r.url, &r.name)
                        });
                        (r, check)
                    });
                    *slots[i].lock().unwrap() = Some(res);
                    tick();
                }
            });
        }
    });
    if tty {
        print!("\r{:40}\r", "");
    }
    slots
        .into_iter()
        .map(|m| m.into_inner().unwrap().unwrap())
        .collect()
}

/// Whether MemTest86 needs a download; forced runs always do.
fn memtest_check(dest: &Path, manifest: &Manifest, args: &Args) -> Check {
    if args.force {
        Check {
            need: true,
            reason: "forced".into(),
            meta: None,
        }
    } else {
        manifest::check_update(dest, manifest.get("memtest"), MEMTEST_URL, MEMTEST_IMG)
    }
}

/// MemTest86 is special: a zip on the server, an .img on the stick.
fn update_memtest(
    dest: &Path,
    manifest: &mut Manifest,
    args: &Args,
    summary: &mut Summary,
    check: Check,
) {
    let name = MEMTEST_IMG.to_string();
    if !check.need {
        summary.push((name, "SKIP (up to date)".into()));
    } else if args.dry_run {
        summary.push((name, "WOULD DOWNLOAD".into()));
    } else {
        match download::download_memtest(dest) {
            Ok(()) => {
                println!("    {}", style::green("done, image extracted"));
                // Record the source zip's size/mtime so reruns detect changes.
                let meta = check.meta.unwrap_or_else(|| net::remote_meta(MEMTEST_URL));
                manifest::record_version(
                    dest,
                    manifest,
                    "memtest",
                    MEMTEST_URL,
                    MEMTEST_IMG,
                    Some(&meta),
                );
                summary.push((name, "OK (updated)".into()));
            }
            Err(e) => {
                println!("    {}", style::red(&format!("FAILED: {e}")));
                summary.push((name, format!("FAIL: {e}")));
            }
        }
    }
}

/// What phase 2 will fetch, what is current, and what could not be resolved.
fn print_plan(todo: &[(&str, String, String, u64)], resolved: usize, failed: &[String]) {
    if todo.is_empty() {
        println!("{}", bold("To download: nothing"));
    } else {
        let bytes: u64 = todo.iter().map(|t| t.3).sum();
        let head = format!(
            "To download ({}, ~{:.1} GB):",
            todo.len(),
            bytes as f64 / 1e9
        );
        println!("{}", bold(&head));
        for (label, name, reason, _) in todo {
            println!("  {label:<24} {}", dim(&format!("{name}  [{reason}]")));
        }
    }
    println!("Up to date: {}", resolved - todo.len());
    for f in failed {
        println!("{}", style::red(f));
    }
}

fn print_summary(summary: &Summary, dest: &Path) {
    if summary.is_empty() {
        println!("Nothing to do.");
        return;
    }
    // Up-to-date items only count; everything else gets a line.
    let shown: Vec<_> = summary
        .iter()
        .filter(|(_, s)| !s.starts_with("SKIP (up to date)"))
        .collect();
    if !shown.is_empty() {
        println!("\n{}", bold("Summary"));
    }
    let width = shown
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    for (name, status) in shown {
        println!("  {name:<width$}  {}", style::status(status));
    }
    // One entry per selected item; deletions of old versions come on top.
    let items: Vec<&str> = summary
        .iter()
        .map(|(_, s)| s.as_str())
        .filter(|s| !s.starts_with("DELETE"))
        .collect();
    let count = |p: &str| items.iter().filter(|s| s.starts_with(p)).count();
    let (ok, current, would) = (count("OK"), count("SKIP (up to date)"), count("WOULD"));
    let (fails, manual) = (count("FAIL"), count("MANUAL"));
    let skipped = items.len() - ok - current - would - fails - manual;
    let deleted = summary
        .iter()
        .filter(|(_, s)| s.starts_with("DELETED"))
        .count();
    let mut parts = vec![format!("{ok} updated"), format!("{current} up to date")];
    for (n, what) in [(would, "would download"), (skipped, "skipped")] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    parts.push(format!("{fails} failed"));
    parts.push(format!("{manual} manual (Windows)"));
    if deleted > 0 {
        parts.push(format!("{deleted} old version(s) deleted"));
    }
    println!("\nDone. {} items: {}.", items.len(), parts.join(" | "));
    if fails > 0 {
        println!(
            "For any FAILED item, download it manually into: {}",
            dest.display()
        );
    }
}
