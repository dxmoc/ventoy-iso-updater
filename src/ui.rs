//! Interactive pickers (Python: _quit .. choose_windows).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::app::{Args, Preset, WinMethod};
use crate::catalog::{
    Item, Kind, Tier, catalog, find, fit, preset_items, stick_size_for, total_bytes,
};
use crate::cleanup::{print_inventory, stick_inventory};
use crate::drives::{detect_usb_drives, wsl_unmounted_drives};
use crate::manifest::Manifest;
use crate::net::fmt_size;
use crate::style::{bold, dim, yellow};

/// Leave cleanly from an interactive menu: nothing has been written yet.
pub fn quit() -> ! {
    println!("Nothing written.");
    crate::app::farewell();
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
            println!("\n  {}", bold(&format!("== {} ==", it.category)));
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
            dim(it.desc)
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

/// Capacity of the stick and what is on it now.
struct Room {
    total: u64,
    free: u64,
    installed: BTreeMap<String, (String, u64)>,
}

impl Room {
    fn of(dest: &Path, manifest: &Manifest) -> Option<Room> {
        // A dry run does not create dest, so ask the nearest folder that exists.
        let dir = dest.ancestors().find(|p| p.is_dir())?;
        Some(Room {
            free: crate::download::free_space(dir)?,
            total: crate::drives::mount_total(&dir.to_string_lossy()),
            installed: stick_inventory(dest, manifest).installed,
        })
    }

    /// (selection at catalog size, everything else that stays on the stick).
    fn plan(&self, sel: &[&Item]) -> (u64, u64) {
        let replaced: u64 = sel
            .iter()
            .filter_map(|it| self.installed.get(it.key).map(|(_, size)| *size))
            .sum();
        let used = self.total - self.free.min(self.total);
        (total_bytes(sel), used.saturating_sub(replaced))
    }

    /// None if `sel` fits, else the stick size it needs.
    fn needs(&self, sel: &[&Item]) -> Option<&'static str> {
        let (sel, others) = self.plan(sel);
        (sel + others > self.total / 100 * 95)
            .then(|| stick_size_for(sel + others).unwrap_or("bigger than 1 TB"))
    }

    /// The fit set: what is on the stick stays, the rest of the room is filled.
    fn fit(&self) -> Vec<&'static Item> {
        let have: Vec<&str> = self
            .installed
            .keys()
            .map(String::as_str)
            .filter(|k| find(k).is_some_and(|it| it.tier != Tier::Windows))
            .collect();
        let have_bytes: u64 = have.iter().map(|k| self.installed[*k].1).sum();
        let fixed = (self.total - self.free.min(self.total)).saturating_sub(have_bytes);
        fit(self.total.saturating_sub(fixed), &have)
    }
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// Top-level 'Fit / Standard / Advanced / Everything / Custom' menu. Returns
/// the selected catalog items in catalog order. A selection that will not fit
/// is flagged; interactively it can be redone, under --preset it only warns
/// (each download refuses on its own once space runs out).
pub fn choose_selection(
    preset: Option<Preset>,
    dest: &Path,
    manifest: &Manifest,
) -> Vec<&'static Item> {
    let interactive = matches!(preset, None | Some(Preset::Custom));
    loop {
        let selection = pick_selection(preset, dest, manifest);
        if fits_or_confirmed(&selection, dest, manifest, interactive) {
            return selection;
        }
    }
}

/// Warn when the selection plus what stays on the stick will not fit, and say
/// which stick size would. True to go on.
pub fn fits_or_confirmed(
    selection: &[&'static Item],
    dest: &Path,
    manifest: &Manifest,
    interactive: bool,
) -> bool {
    let Some(room) = Room::of(dest, manifest) else {
        return true;
    };
    let Some(size) = room.needs(selection) else {
        return true;
    };
    let (sel, others) = room.plan(selection);
    let mut msg = format!("\nYour selection: ~{}", gb(sel));
    if others > 0 {
        msg += &format!(
            ", plus ~{} of other files that stay on the stick: ~{} in total",
            gb(others),
            gb(sel + others)
        );
    }
    msg += &format!(". The stick holds {}.", gb(room.total));
    println!("{}", yellow(&msg));
    println!("{}", yellow(&format!("Recommended stick size: {size}")));
    !interactive
        || matches!(
            ask("Continue anyway? [y/N]: ").to_lowercase().as_str(),
            "y" | "yes" | "j" | "ja"
        )
}

fn pick_selection(preset: Option<Preset>, dest: &Path, manifest: &Manifest) -> Vec<&'static Item> {
    let none = HashSet::new();
    if preset == Some(Preset::Custom) {
        return custom_select(&none, dest, manifest);
    }
    let installed = stick_inventory(dest, manifest).installed;
    let room = Room::of(dest, manifest);
    let fit_set = room.as_ref().map(Room::fit);
    match (preset, &room, &fit_set) {
        (Some(Preset::Fit), Some(room), Some(set)) => {
            println!(
                "Stick: {}, {} free -> {} items, ~{}",
                gb(room.total),
                gb(room.free),
                set.len(),
                gb(total_bytes(set))
            );
            return set.clone();
        }
        (Some(Preset::Fit), ..) => {
            // A script must not mistake "checked nothing" for success.
            eprintln!("Cannot read the free space of {}.", dest.display());
            crate::app::farewell();
            std::process::exit(1);
        }
        (Some(p), ..) => return preset_items(p),
        (None, ..) => {}
    }

    let entry = |key: &str, head: String, what: &str, mark: String| {
        println!("  [{key}] {head:<36} {}{mark}", dim(what));
    };
    let preset = |key, p, name: &str, what| {
        let items = preset_items(p);
        let size = total_bytes(&items);
        let mark = match room.as_ref().and_then(|r| r.needs(&items)) {
            Some(stick) => yellow(&format!("  needs a {stick} stick")),
            None => String::new(),
        };
        let head = format!(
            "{name:<11}({} items, ~{:.0} GB)",
            items.len(),
            size as f64 / 1e9
        );
        entry(key, head, what, mark);
    };
    println!();
    if let Some(room) = &room {
        println!("Stick: {}, {} free", gb(room.total), gb(room.free));
    }
    println!("{}\n", bold("What should go on the stick?"));
    if let Some(set) = &fit_set {
        let head = format!(
            "Fit this stick ({} ISOs, ~{:.0} GB)",
            set.len(),
            total_bytes(set) as f64 / 1e9
        );
        entry("0", head, "the best mix that fits", String::new());
    }
    preset("1", Preset::Standard, "Standard", "one solid pick per job");
    preset(
        "2",
        Preset::Advanced,
        "Advanced",
        "+ sibling releases & more tools",
    );
    preset(
        "3",
        Preset::Everything,
        "Everything",
        "+ niche and legacy items",
    );
    entry("4", "Custom".into(), "pick individually", String::new());
    entry(
        "5",
        "Custom from Standard".into(),
        "start with Standard, then adjust",
        String::new(),
    );
    println!("  [q] quit");
    if !installed.is_empty() {
        entry(
            "6",
            format!("Update what's there ({} items)", installed.len()),
            "keep the current set, just refresh it",
            String::new(),
        );
        println!(
            "\n  -> Been here before? Take [6] first: it brings the stick up to date without\n     \
                  adding anything new. Only picking a preset on a stick this program has\n     \
                  already filled risks queueing ISOs you never wanted."
        );
    }
    println!();
    // A filled stick defaults to refreshing what is there: a fresh preset or
    // fit would queue ISOs the user never picked.
    let default = if !installed.is_empty() {
        "6"
    } else if room.is_some() {
        "0"
    } else {
        "1"
    };
    let choice = ask(&format!("Select [{default}]: ")).to_lowercase();
    let choice = if choice.is_empty() { default } else { &choice };

    match (choice, fit_set) {
        ("q", _) => quit(),
        ("0", Some(set)) => set,
        ("1", _) => preset_items(Preset::Standard),
        ("2", _) => preset_items(Preset::Advanced),
        ("3", _) => preset_items(Preset::Everything),
        ("4", _) => custom_select(&none, dest, manifest),
        ("5", _) => {
            let std: HashSet<&str> = preset_items(Preset::Standard)
                .iter()
                .map(|it| it.key)
                .collect();
            custom_select(&std, dest, manifest)
        }
        // Start from what the stick holds: the usual "just bring my stick up
        // to date" case, still adjustable before it runs.
        ("6", _) if !installed.is_empty() => custom_select(
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
