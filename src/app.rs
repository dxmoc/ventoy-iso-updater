//! CLI and the top-level run (Python: main()).

use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use clap::{Parser, ValueEnum};

use crate::catalog::{Item, Kind, Resolved};
use crate::manifest::{Check, Manifest};
use crate::{Summary, autoinstall, cleanup, download, manifest, net, ui, windows};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Preset {
    Standard,
    Advanced,
    Everything,
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
fn farewell() {
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
        "Version manifest: {}",
        dest.join(crate::MANIFEST_NAME).display()
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

    // ---- phase 1: detect newest versions ----
    println!("Checking for latest versions...\n");
    let mut summary: Summary = Vec::new();
    let mut resolved: Vec<(String, String, Resolved)> = Vec::new();
    let mut checked: HashMap<&str, Check> = HashMap::new();
    for (item, res) in iso_items
        .iter()
        .zip(detect_all(&iso_items, &dest, &manifest, args.force))
    {
        match res {
            Err(e) => {
                println!("  {:<24} -> DETECT FAILED ({e})", item.label);
                summary.push((item.label.to_string(), format!("FAIL (detect): {e}")));
            }
            Ok((r, check)) => {
                let extra = if r.mirrors.is_empty() {
                    String::new()
                } else {
                    format!("  (+{} mirrors)", r.mirrors.len())
                };
                println!("  {:<24} -> {}{extra}", item.label, r.name);
                if let Some(c) = check {
                    checked.insert(item.key, c);
                }
                resolved.push((item.key.to_string(), item.label.to_string(), r));
            }
        }
    }
    if memtest {
        println!("  {:<24} -> {MEMTEST_IMG} (latest zip)", "MemTest86");
    }
    if !win_items.is_empty() {
        let how = match method {
            WinMethod::Uup => "UUP dump (official, local build)",
            WinMethod::Fido => "Fido (official MS link)",
            WinMethod::Massgrave => "massgrave hand-off",
        };
        println!(
            "  {:<24} -> {} ISO(s) via {how}",
            "Windows",
            win_items.len()
        );
    }
    println!();

    // ---- phase 2: sequential downloads (only what changed) ----
    println!("Starting sequential downloads...\n");
    for (key, label, r) in &resolved {
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
            checked.remove(key.as_str()),
        );
    }
    if memtest {
        update_memtest(&dest, &mut manifest, &args, &mut summary);
    }

    // Every Windows method drives a browser or builds an image locally, so a
    // dry run has nothing to report without doing the work.
    if !win_items.is_empty() && args.dry_run {
        println!(
            "==> {:<24} {} item(s) skipped in --dry-run\n",
            "Windows",
            win_items.len()
        );
    } else if !win_items.is_empty() {
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
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|m| m.into_inner().unwrap().unwrap())
        .collect()
}

/// MemTest86 is special: a zip on the server, an .img on the stick.
fn update_memtest(dest: &Path, manifest: &mut Manifest, args: &Args, summary: &mut Summary) {
    let check = if args.force {
        Check {
            need: true,
            reason: "forced".into(),
            meta: None,
        }
    } else {
        manifest::check_update(dest, manifest.get("memtest"), MEMTEST_URL, MEMTEST_IMG)
    };
    println!("==> {:<24} {MEMTEST_IMG}  [{}]", "MemTest86", check.reason);
    let name = MEMTEST_IMG.to_string();
    if args.dry_run {
        summary.push((
            name,
            if check.need {
                "WOULD DOWNLOAD"
            } else {
                "SKIP (up to date)"
            }
            .into(),
        ));
        println!();
    } else if !check.need {
        summary.push((name, "SKIP (up to date)".into()));
        println!();
    } else {
        match download::download_memtest(dest) {
            Ok(()) => {
                println!("    ok\n");
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
                println!("    FAILED: {e}\n");
                summary.push((name, format!("FAIL: {e}")));
            }
        }
    }
}

fn print_summary(summary: &Summary, dest: &Path) {
    if summary.is_empty() {
        println!("Nothing to do.");
        return;
    }
    println!("\n{} SUMMARY {}", "=".repeat(16), "=".repeat(16));
    let width = summary
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    for (name, status) in summary {
        println!("  {name:<width$}  {status}");
    }
    let count = |p: &str| summary.iter().filter(|(_, s)| s.starts_with(p)).count();
    let fails = count("FAIL");
    println!(
        "\nDone. {} items | {} updated | {} up-to-date | {fails} failed | {} manual (Windows).",
        summary.len(),
        count("OK"),
        count("SKIP"),
        count("MANUAL")
    );
    if fails > 0 {
        println!(
            "For any FAILED item, download it manually into: {}",
            dest.display()
        );
    }
}
