//! Interactive pickers (Python: _quit .. choose_windows).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::app::{Args, Preset, WinMethod};
use crate::catalog::{Item, Kind, catalog, preset_items};
use crate::cleanup::{print_inventory, stick_inventory};
use crate::drives::{detect_usb_drives, wsl_unmounted_drives};
use crate::manifest::Manifest;
use crate::net::fmt_size;

/// Leave cleanly from an interactive menu: nothing has been written yet.
pub fn quit() -> ! {
    println!("Nothing written.");
    std::process::exit(0)
}

/// Python input(): prompt, then one trimmed line. EOF or a read error count
/// as an empty answer, i.e. the default.
pub fn ask(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(_) => line.trim().to_string(),
        Err(_) => String::new(),
    }
}

/// str.isdigit() + int(); huge numbers saturate instead of failing.
fn number(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().unwrap_or(u64::MAX))
}

/// Interactive target picker used when --dest is not supplied.
pub fn choose_dest() -> PathBuf {
    let drives = detect_usb_drives();
    println!("Where should the ISOs go?\n");
    for (i, (path, label, total)) in drives.iter().enumerate() {
        let size = if *total > 0 {
            fmt_size(*total)
        } else {
            "?".to_string()
        };
        println!("  [{}] {path}  {label:<20}  {size}", i + 1);
    }
    if drives.is_empty() {
        println!("  (no removable USB drives detected)");
    }
    // WSL automounts only what was plugged in when it started; say so instead
    // of letting the user conclude the drive is broken.
    for (letter, label) in wsl_unmounted_drives() {
        let up = letter.to_uppercase();
        println!("  note: Windows drive {up}: ({label}) is not mounted in WSL yet:");
        println!("        sudo mkdir -p /mnt/{letter} && sudo mount -t drvfs {up}: /mnt/{letter}");
    }
    println!("  [o] other path (type it in)");
    println!("  [.] default folder ./isos");
    println!("  [q] quit\n");

    // The hint tracks what an empty answer does: the first drive if there is
    // one, otherwise ./isos.
    let default = if drives.is_empty() { "." } else { "1" };
    let choice = ask(&format!("Select target [{default}]: ")).to_lowercase();
    let isos = || PathBuf::from("./isos");
    match choice.as_str() {
        "q" => quit(),
        "" if !drives.is_empty() => PathBuf::from(&drives[0].0),
        "" | "." => isos(),
        "o" => {
            let manual = ask("Path: ");
            let manual = manual.trim_matches('"');
            if manual.is_empty() {
                isos()
            } else {
                PathBuf::from(manual)
            }
        }
        c => match number(c).filter(|&i| i >= 1 && i <= drives.len() as u64) {
            Some(i) => PathBuf::from(&drives[i as usize - 1].0),
            None => {
                println!("Unrecognized choice -> using ./isos");
                isos()
            }
        },
    }
}

/// Turn '1,3,5-9' into a sorted list of valid 1-based indices.
pub fn parse_ranges(raw: &str, n: usize) -> Vec<usize> {
    let mut picks = BTreeSet::new();
    for part in raw.replace(' ', "").split(',') {
        if let Some((a, b)) = part.split_once('-') {
            if let (Some(a), Some(b)) = (number(a), number(b)) {
                picks.extend(a.max(1)..=b.min(n as u64));
            }
        } else if let Some(i) = number(part).filter(|&i| i >= 1 && i <= n as u64) {
            picks.insert(i);
        }
    }
    picks.into_iter().map(|i| i as usize).collect()
}

/// 0-based indices (catalog order) for a picker answer other than 'q'.
fn apply_selection(raw: &str, keys: &[&str], preselect: &HashSet<&str>) -> Vec<usize> {
    if raw == "a" || raw == "all" {
        return (0..keys.len()).collect();
    }
    let mut chosen: Vec<bool> = keys.iter().map(|k| preselect.contains(k)).collect();
    if raw.starts_with(['+', '-']) {
        // incremental edits against the preset
        let token = regex::Regex::new(r"[+-][0-9,\-]+").unwrap();
        for tok in token.find_iter(raw) {
            let add = tok.as_str().starts_with('+');
            for i in parse_ranges(&tok.as_str()[1..], keys.len()) {
                chosen[i - 1] = add;
            }
        }
    } else if !raw.is_empty() {
        // plain list -> exact selection
        return parse_ranges(raw, keys.len())
            .into_iter()
            .map(|i| i - 1)
            .collect();
    }
    (0..keys.len()).filter(|&i| chosen[i]).collect()
}

/// Numbered, category-grouped picker. `preselect` keys start as [x]; items
/// already on the stick are listed first and marked '*', so it is obvious
/// what would be a re-download and what is missing.
fn custom_select(
    preselect: &HashSet<&str>,
    dest: &Path,
    manifest: &Manifest,
) -> Vec<&'static Item> {
    let cat = catalog();
    let numbers: BTreeMap<String, usize> = cat
        .iter()
        .enumerate()
        .map(|(i, it)| (it.key.to_string(), i + 1))
        .collect();
    let inv = stick_inventory(dest, manifest);
    print_inventory(dest, &inv, Some(&numbers));

    println!("\nPick items individually. Enter numbers/ranges, e.g. 1,3,5-9.");
    println!(
        "Prefix with '-' to remove from the preset, '+' to add. 'a'=all, ''=keep preset, 'q'=quit."
    );
    println!(
        "  [x] = selected    * = already on the stick (re-picking it only checks for updates)"
    );
    let mut last = None;
    for (i, it) in cat.iter().enumerate() {
        if last != Some(it.category) {
            println!("\n  == {} ==", it.category);
            last = Some(it.category);
        }
        let mark = if preselect.contains(it.key) { 'x' } else { ' ' };
        let have = if inv.installed.contains_key(it.key) {
            '*'
        } else {
            ' '
        };
        println!(
            "   [{mark}] {have} {:2}  {:<24} {}",
            i + 1,
            it.label,
            it.desc
        );
    }

    let raw = ask("\nSelection: ").to_lowercase();
    if raw == "q" {
        quit();
    }
    let keys: Vec<&str> = cat.iter().map(|it| it.key).collect();
    apply_selection(&raw, &keys, preselect)
        .into_iter()
        .map(|i| &cat[i])
        .collect()
}

/// Top-level 'Standard / Advanced / Everything / Custom' menu. Returns the
/// selected catalog items in catalog order.
pub fn choose_selection(
    preset: Option<Preset>,
    dest: &Path,
    manifest: &Manifest,
) -> Vec<&'static Item> {
    let none = HashSet::new();
    match preset {
        Some(Preset::Custom) => return custom_select(&none, dest, manifest),
        Some(p) => return preset_items(p),
        None => {}
    }
    let installed = stick_inventory(dest, manifest).installed;

    let count = |p| preset_items(p).len();
    println!("\nWhat should go on the stick?\n");
    println!(
        "  [1] Standard    ({:2} items)  one solid pick per job",
        count(Preset::Standard)
    );
    println!(
        "  [2] Advanced    ({:2} items)  + sibling releases & more tools",
        count(Preset::Advanced)
    );
    println!(
        "  [3] Everything  ({:2} items)  + niche and legacy items",
        count(Preset::Everything)
    );
    println!("  [4] Custom                 pick individually");
    println!("  [5] Custom from Standard   start with Standard, then adjust");
    println!("  [q] quit");
    if !installed.is_empty() {
        println!(
            "  [6] Update what's there ({:2} items)  keep the current set, just refresh it",
            installed.len()
        );
        println!(
            "\n  -> Been here before? Take [6] first: it brings the stick up to date without\n     \
                  adding anything new. Only picking a preset on a stick this script has\n     \
                  already filled risks queueing ISOs you never wanted."
        );
    }
    println!();
    let choice = ask("Select [1]: ").to_lowercase();

    match choice.as_str() {
        "q" => quit(),
        "" | "1" => preset_items(Preset::Standard),
        "2" => preset_items(Preset::Advanced),
        "3" => preset_items(Preset::Everything),
        "4" => custom_select(&none, dest, manifest),
        "5" => {
            let std: HashSet<&str> = preset_items(Preset::Standard)
                .iter()
                .map(|it| it.key)
                .collect();
            custom_select(&std, dest, manifest)
        }
        // Start from what the stick holds: the usual "just bring my stick up
        // to date" case, still adjustable before it runs.
        "6" if !installed.is_empty() => custom_select(
            &installed.keys().map(String::as_str).collect(),
            dest,
            manifest,
        ),
        _ => {
            println!("Unrecognized choice -> Standard");
            preset_items(Preset::Standard)
        }
    }
}

/// (method, headline, what it does, upside, downside). Menu order, so the
/// recommended method is first and a bare Enter selects it.
const WINDOWS_METHODS: [(WinMethod, &str, &str, &str, &str); 3] = [
    (
        WinMethod::Massgrave,
        "massgrave  (recommended)",
        "Opens the massgrave.dev page, tells you which entry to click, then \
      watches your Downloads folder and moves the ISO onto the stick.",
        "always works, no admin rights, nothing upstream can block it",
        "you click the download yourself, so the run is not unattended",
    ),
    (
        WinMethod::Uup,
        "uup",
        "Pulls the build straight from Windows Update and assembles the ISO \
      locally with DISM.",
        "official and unattended, no bot protection in the way",
        "Windows only, needs admin, ~40 minutes and ~15 GB of free space",
    ),
    (
        WinMethod::Fido,
        "fido",
        "Resolves an official, direct Microsoft download link and fetches it.",
        "hands-off and light: no admin, no local build, no browser",
        "Windows only, and Microsoft's anti-bot 'Sentinel' blocks it unpredictably",
    ),
];

/// textwrap.wrap for plain prose: greedy on whitespace.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(cur) if cur.chars().count() + 1 + word.chars().count() <= width => {
                cur.push(' ');
                cur.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

fn windows_methods_menu(on_windows: bool) -> String {
    let mut s = String::from("\nHow should the Windows ISOs be fetched?\n\n");
    for (i, (method, headline, what, good, bad)) in WINDOWS_METHODS.iter().enumerate() {
        let unavailable = *method != WinMethod::Massgrave && !on_windows;
        s += &format!(
            "  [{}] {headline}{}\n",
            i + 1,
            if unavailable {
                "   (unavailable here: needs Windows)"
            } else {
                ""
            }
        );
        for line in wrap(what, 68) {
            s += &format!("      {line}\n");
        }
        for (sign, text) in [('+', good), ('-', bad)] {
            for (n, line) in wrap(text, 66).iter().enumerate() {
                s += &format!("      {}  {line}\n", if n == 0 { sign } else { ' ' });
            }
        }
        s.push('\n');
    }
    s
}

/// Ask how the Windows ISOs should be fetched. None of the three is simply
/// better, so the trade-off is spelled out; the default is the one that cannot
/// fail for reasons outside the user's control, at the price of one click.
fn choose_windows_method() -> WinMethod {
    print!("{}", windows_methods_menu(cfg!(windows)));
    let raw = ask("Method [1]: ");
    match number(&raw).filter(|&i| i >= 1 && i <= WINDOWS_METHODS.len() as u64) {
        Some(i) => WINDOWS_METHODS[i as usize - 1].0,
        None => WINDOWS_METHODS[0].0,
    }
}

/// Opt-in prompt for Windows ISOs; also settles args.windows if still None.
/// They are in no preset because no method runs unattended; an explicit
/// --windows wins, the method prompt only appears without it.
pub fn choose_windows(already: &HashSet<&str>, args: &mut Args) -> Vec<&'static Item> {
    let wins: Vec<&'static Item> = catalog()
        .iter()
        .filter(|it| matches!(it.kind, Kind::Windows(_)))
        .collect();
    if wins.iter().any(|it| already.contains(it.key)) {
        return Vec::new(); // already picked in the custom selector
    }

    println!("\nWindows ISOs?  (none of the ways to get them is fully unattended;");
    println!("you pick which one to use in the next step)");
    let ans = ask("Add Windows ISOs? [y/N]: ").to_lowercase();
    if !matches!(ans.as_str(), "y" | "yes" | "j" | "ja") {
        return Vec::new();
    }

    println!();
    for (i, it) in wins.iter().enumerate() {
        println!("  [{}] {:<24} {}", i + 1, it.label, it.desc);
    }
    let raw = ask("Which? (e.g. 1,3 -- empty = 1,2,3): ");
    let idx = if raw.is_empty() {
        (1..=wins.len().min(3)).collect()
    } else {
        parse_ranges(&raw, wins.len())
    };
    let picked: Vec<&'static Item> = idx.into_iter().map(|i| wins[i - 1]).collect();

    if !picked.is_empty() && args.windows.is_none() {
        args.windows = Some(choose_windows_method());
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_ranges("1,3,5-9", 20), [1, 3, 5, 6, 7, 8, 9]);
        assert_eq!(parse_ranges(" 3 , 1,1,,0,21,x,4-2", 20), [1, 3]);
        assert_eq!(parse_ranges("18-99999999999999999999999", 20), [18, 19, 20]);
        assert_eq!(parse_ranges("0-2,5-5-7,+4", 20), [1, 2]);
        assert!(parse_ranges("", 5).is_empty());
    }

    #[test]
    fn selection_answers() {
        let keys = ["a", "b", "c", "d", "e"];
        let pre: HashSet<&str> = ["b", "d", "zz"].into();
        assert_eq!(apply_selection("", &keys, &pre), [1, 3]);
        assert_eq!(apply_selection("all", &keys, &pre), [0, 1, 2, 3, 4]);
        assert_eq!(apply_selection("5,1", &keys, &pre), [0, 4]);
        assert_eq!(apply_selection("+1,5-4", &keys, &pre), [0, 1, 3]);
        assert_eq!(apply_selection("-4+1-2", &keys, &pre), [0, 1]); // "-4", "+1-2"
        assert_eq!(apply_selection("+5 -2", &keys, &pre), [3, 4]);
        assert_eq!(apply_selection("+1, 3", &keys, &pre), [0, 1, 3]); // the space ends the token
    }

    /// Expected text captured from the Python choose_windows_method on Linux.
    #[test]
    fn windows_menu_matches_python() {
        let want = "\nHow should the Windows ISOs be fetched?\n\n  [1] massgrave  (recommended)\n      Opens the massgrave.dev page, tells you which entry to click, then\n      watches your Downloads folder and moves the ISO onto the stick.\n      +  always works, no admin rights, nothing upstream can block it\n      -  you click the download yourself, so the run is not unattended\n\n  [2] uup   (unavailable here: needs Windows)\n      Pulls the build straight from Windows Update and assembles the ISO\n      locally with DISM.\n      +  official and unattended, no bot protection in the way\n      -  Windows only, needs admin, ~40 minutes and ~15 GB of free space\n\n  [3] fido   (unavailable here: needs Windows)\n      Resolves an official, direct Microsoft download link and fetches it.\n      +  hands-off and light: no admin, no local build, no browser\n      -  Windows only, and Microsoft's anti-bot 'Sentinel' blocks it\n         unpredictably\n\n";
        assert_eq!(windows_methods_menu(false), want);
        assert!(!windows_methods_menu(true).contains("unavailable"));
    }
}
