//! Superseded-version sweep and stick inventory
//! (Python: _family_regex .. print_inventory).
//!
//! fetch_tracked() only deletes the file the manifest recorded for that exact
//! item. Anything the manifest lost track of -- an ISO from before the manifest
//! existed, one downloaded by hand, one left behind by an aborted run -- would
//! stay on the stick forever. This sweep finds those leftovers, but only when
//! it can name the newer file that replaced them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

use regex::Regex;

use crate::app::Cleanup;
use crate::catalog::{self, Resolved};
use crate::manifest::Manifest;
use crate::net::fmt_size;
use crate::{MANIFEST_NAME, Summary};

/// key -> (label, filename that should be on the stick).
pub type Current = BTreeMap<String, (String, String)>;

/// What is on the stick: catalog key -> (filename, size), plus unaccounted images.
#[derive(Debug, Default)]
pub struct Inventory {
    pub installed: BTreeMap<String, (String, u64)>,
    pub others: Vec<(String, u64)>,
}

const SWEEP_EXTS: [&str; 2] = [".iso", ".img"];

fn is_image(name: &str) -> bool {
    let lower = name.to_lowercase();
    SWEEP_EXTS.iter().any(|e| lower.ends_with(e))
}

/// Sorted file names in `dir` (non-UTF-8 names cannot be catalog files).
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|rd| rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Regex matching other versions of the same ISO, capturing the numbers.
///
/// Every run of digits/dots in the stem becomes a capture group, everything
/// else stays literal: "omarchy-3.8.4.iso" -> "omarchy-([\d.]+)\.iso". That
/// matches 3.8.3 but not "omarchy-dev.iso", and -- unlike a prefix match --
/// not a different product that merely starts with the same word.
fn family_regex(name: &str) -> Regex {
    // os.path.splitext: the last dot, unless only dots precede it.
    let (stem, ext) = match name.rfind('.') {
        Some(i) if name[..i].trim_start_matches('.').is_empty() => (name, ""),
        Some(i) => name.split_at(i),
        None => (name, ""),
    };
    let runs = Regex::new(r"[0-9.]+").unwrap();
    let (mut body, mut at) = (String::new(), 0);
    for m in runs.find_iter(stem) {
        body += &regex::escape(&stem[at..m.start()]);
        body += r"([0-9.]+)";
        at = m.end();
    }
    body += &regex::escape(&stem[at..]);
    Regex::new(&format!("(?i)^{body}{}$", regex::escape(ext))).unwrap()
}

/// Per-position version numbers of `name` within its family, or None.
///
/// Comparing these instead of every integer in the filename keeps constants
/// out of the comparison: "ubuntu-24.04-..-amd64" vs "ubuntu-24.04.3-..-amd64"
/// must hinge on 24.04 < 24.04.3, not on the amd64 that follows it.
fn version_tokens(rgx: &Regex, name: &str) -> Option<Vec<Vec<u64>>> {
    // Python flattens verkey() of each group, i.e. all digit runs in order.
    let caps = rgx.captures(name)?;
    Some(caps.iter().skip(1).flatten()
        .map(|g| g.as_str().split('.').filter(|s| !s.is_empty()).map(|n| n.parse().unwrap_or(u64::MAX)).collect())
        .collect())
}

/// How many leading version numbers two token lists agree on -- used to pick
/// the closest relative when several items claim the same file.
fn shared_prefix(a: &[Vec<u64>], b: &[Vec<u64>]) -> usize {
    let mut n = 0;
    for (ta, tb) in a.iter().zip(b) {
        for (x, y) in ta.iter().zip(tb) {
            if x != y {
                return n;
            }
            n += 1;
        }
    }
    n
}

/// key -> (label, filename that *should* be on the stick) for everything we
/// know about: this run's resolved items plus every manifest record (so items
/// not selected this run are still recognized, not swept).
/// `resolved`: (key, label, Resolved) of this run; `extra`: (key, label, filename).
pub fn current_files(manifest: &Manifest, resolved: &[(String, String, Resolved)], extra: &[(&str, &str, &str)]) -> Current {
    let mut cur = Current::new();
    for key in manifest.keys() {
        if let Some(name) = crate::manifest::entry_filename(manifest, key) {
            let label = catalog::find(key).map_or(key.as_str(), |it| it.label);
            cur.insert(key.clone(), (label.to_string(), name.to_string()));
        }
    }
    for (key, label, r) in resolved {
        cur.insert(key.clone(), (label.clone(), r.name.clone()));
    }
    for (key, label, name) in extra {
        cur.insert(key.to_string(), (label.to_string(), name.to_string()));
    }
    cur
}

/// [(label, old_filename, size, new_filename)] of replaced ISOs on disk.
///
/// A file is only reported when exactly one item claims it -- either because
/// the manifest recorded it under that item before a rename, or because it
/// matches that item's filename family *and* carries a lower version number.
/// Ambiguous matches, newer files and anything currently in use are left
/// alone: this deletes multi-GB downloads, so it errs towards keeping.
fn find_superseded(dest: &Path, manifest: &Manifest, current: &Current) -> Vec<(String, String, u64, String)> {
    let mut protected: HashSet<String> = current.values().map(|(_, n)| n.to_lowercase()).collect();
    protected.insert(MANIFEST_NAME.to_lowercase());
    let history: HashMap<&str, HashSet<String>> = manifest.iter()
        .filter(|(_, e)| e.is_object())
        .map(|(k, e)| {
            let seen = e.get("history").and_then(|h| h.as_array())
                .map(|h| h.iter().filter_map(|s| s.as_str()).map(str::to_lowercase).collect())
                .unwrap_or_default();
            (k.as_str(), seen)
        })
        .collect();
    let families: Vec<(&String, Regex)> = current.iter().map(|(k, (_, n))| (k, family_regex(n))).collect();

    let mut found = Vec::new();
    for fname in listing(dest) {
        // A ".part" of a superseded ISO is dead weight too, but one belonging to
        // a current file is a resumable download -- keep it.
        let base = match fname.len().checked_sub(5).and_then(|i| fname.get(i..).map(|t| (i, t))) {
            Some((i, tail)) if tail.eq_ignore_ascii_case(".part") => &fname[..i],
            _ => fname.as_str(),
        };
        let lower = base.to_lowercase();
        if !is_image(base) || protected.contains(&lower) {
            continue;
        }
        let path = dest.join(&fname);
        if !path.is_file() {
            continue;
        }
        let mut owners: Vec<&str> = history.iter().filter(|(_, seen)| seen.contains(&lower)).map(|(k, _)| *k).collect();
        if owners.is_empty() {
            // Every item whose family matches and whose current file is newer.
            // Several can match (Ubuntu 24.04 vs 26.04 desktop); the closest
            // version wins, and a genuine tie is left alone.
            let claims: Vec<(usize, &str)> = families.iter().filter_map(|(k, rgx)| {
                let tok = version_tokens(rgx, base)?;
                let cur = version_tokens(rgx, &current[*k].1)?;
                (tok < cur).then(|| (shared_prefix(&tok, &cur), k.as_str()))
            }).collect();
            if let Some(best) = claims.iter().map(|c| c.0).max() {
                owners = claims.iter().filter(|c| c.0 == best).map(|c| c.1).collect();
            }
        }
        if owners.len() != 1 {
            continue;
        }
        let (label, newer) = &current[owners[0]];
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        found.push((label.clone(), fname, size, newer.clone()));
    }
    found
}

/// List superseded ISOs and, once confirmed, delete them.
pub fn cleanup_superseded(dest: &Path, manifest: &mut Manifest, current: &Current, mode: Cleanup, summary: &mut Summary) {
    let old = find_superseded(dest, manifest, current);
    if old.is_empty() {
        return;
    }
    let total: u64 = old.iter().map(|o| o.2).sum();
    let width = old.iter().map(|o| o.1.chars().count()).max().unwrap_or(0);

    println!("\n{0} OLD VERSIONS {0}", "-".repeat(15));
    println!("Superseded by a newer release, still taking up space:\n");
    for (label, name, size, newer) in &old {
        println!("  {label}");
        println!("    old: {name:<width$}  {}", fmt_size(*size));
        println!("    new: {newer}");
    }
    println!("\n  {} file(s), {} total", old.len(), fmt_size(total));

    match mode {
        // Also the dry-run path, which forces this mode -- so word it for both
        // instead of naming a flag the user may not have passed.
        Cleanup::No => {
            println!("  (keeping them -- pass --cleanup yes to delete)");
            return;
        }
        Cleanup::Ask => {
            print!("\nDelete these {} file(s)? [y/N]: ", old.len());
            let _ = io::stdout().flush();
            let mut ans = String::new();
            let _ = io::stdin().lock().read_line(&mut ans);
            if !["y", "yes", "j", "ja"].contains(&ans.trim().to_lowercase().as_str()) {
                println!("Keeping them.");
                return;
            }
        }
        Cleanup::Yes => {}
    }

    println!();
    let mut freed = 0;
    for (label, name, size, _) in &old {
        match fs::remove_file(dest.join(name)) {
            Ok(()) => {
                freed += size;
                println!("  deleted {name}");
                summary.push((name.clone(), format!("DELETED (old version of {label})")));
            }
            Err(e) => {
                println!("  could NOT delete {name}: {e}");
                summary.push((name.clone(), format!("DELETE FAILED: {e}")));
            }
        }
    }
    println!("\nFreed {}.", fmt_size(freed));
}

/// What is on the stick right now. `installed`: every manifest record whose file
/// is really there; `others`: images the manifest doesn't account for. Purely
/// local -- no network, so the pickers can show it before anything is resolved.
pub fn stick_inventory(dest: &Path, manifest: &Manifest) -> Inventory {
    let mut inv = Inventory::default();
    let mut claimed = HashSet::new();
    for key in manifest.keys() {
        let Some(name) = crate::manifest::entry_filename(manifest, key) else { continue };
        claimed.insert(name.to_lowercase());
        // Recorded but gone -> not installed.
        if let Ok(md) = fs::metadata(dest.join(name)) {
            inv.installed.insert(key.clone(), (name.to_string(), md.len()));
        }
    }
    for fname in listing(dest) {
        if !is_image(&fname) || claimed.contains(&fname.to_lowercase()) {
            continue;
        }
        if let Ok(md) = fs::metadata(dest.join(&fname)) {
            inv.others.push((fname, md.len()));
        }
    }
    inv
}

/// Show what the stick already holds, so nothing gets picked twice.
/// `numbers`: key -> catalog number shown in front of each line.
pub fn print_inventory(dest: &Path, inv: &Inventory, numbers: Option<&BTreeMap<String, usize>>) {
    if inv.installed.is_empty() && inv.others.is_empty() {
        println!("\nNothing on {} yet.", dest.display());
        return;
    }
    let total: u64 = inv.installed.values().map(|v| v.1).chain(inv.others.iter().map(|o| o.1)).sum();
    println!("\nAlready on {} -- {} item(s), {}:\n", dest.display(), inv.installed.len() + inv.others.len(), fmt_size(total));
    let cat = catalog::catalog();
    let order = |k: &str| cat.iter().position(|it| it.key == k).unwrap_or(1_000_000);
    let mut keys: Vec<&String> = inv.installed.keys().collect();
    keys.sort_by_key(|k| order(k));
    for key in keys {
        let (name, size) = &inv.installed[key];
        let num = numbers.and_then(|n| n.get(key)).map(ToString::to_string).unwrap_or_default();
        let label = catalog::find(key).map_or(key.as_str(), |it| it.label);
        println!("  {num:>3}  {label:<24} {name:<42} {:>9}", fmt_size(*size));
    }
    for (name, size) in &inv.others {
        println!("  {:>3}  {:<24} {name:<42} {:>9}", "", "(not in the catalog)", fmt_size(*size));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cur(items: &[(&str, &str)]) -> Current {
        items.iter().map(|(k, n)| (k.to_string(), (format!("L-{k}"), n.to_string()))).collect()
    }

    #[test]
    fn family_and_tokens() {
        let r = family_regex("omarchy-3.8.4.iso");
        assert_eq!(r.as_str(), r"(?i)^omarchy\-([0-9.]+)\.iso$");
        assert!(r.is_match("OMARCHY-3.8.3.ISO"));
        assert!(!r.is_match("omarchy-dev.iso"));
        let u = family_regex("ubuntu-24.04.3-desktop-amd64.iso");
        assert_eq!(version_tokens(&u, "ubuntu-24.04-desktop-amd64.iso"), Some(vec![vec![24, 4], vec![64]]));
        assert!(version_tokens(&u, "ubuntu-24.04-desktop-amd64.iso") < version_tokens(&u, "ubuntu-24.04.3-desktop-amd64.iso"));
        assert!(version_tokens(&u, "ubuntu-24.04-server-amd64.iso").is_none());
        assert_eq!(shared_prefix(&[vec![24, 4, 3]], &[vec![24, 4, 1]]), 2);
        // Like Python: splitext keeps the dot, and re.split turns it into a group.
        assert_eq!(family_regex(".hidden").as_str(), r"(?i)^([0-9.]+)hidden$");
    }

    #[test]
    fn superseded_detection() {
        let d = std::env::temp_dir().join(format!("vi_cleanup_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        for f in ["omarchy-3.8.3.iso", "omarchy-3.8.4.iso", "omarchy-3.9.0.iso", "omarchy-dev.iso",
                  "ubuntu-24.04-desktop-amd64.iso", "ubuntu-26.04-desktop-amd64.iso", "ubuntu-24.04.3-desktop-amd64.iso",
                  "ubuntu-24.04.3-desktop-amd64.iso.part", "omarchy-3.8.2.iso.part", "renamed-old.img", "notes.txt",
                  MANIFEST_NAME] {
            fs::write(d.join(f), b"xx").unwrap();
        }
        let manifest: Manifest = serde_json::from_str(r#"{"other": {"filename": "new-name.img", "history": ["Renamed-Old.img"]}}"#).unwrap();
        let current = cur(&[("omarchy", "omarchy-3.8.4.iso"), ("u24", "ubuntu-24.04.3-desktop-amd64.iso"),
                            ("u26", "ubuntu-26.04-desktop-amd64.iso"), ("other", "new-name.img")]);
        let found: Vec<(String, String)> = find_superseded(&d, &manifest, &current).into_iter()
            .map(|(label, name, size, newer)| { assert_eq!(size, 2); (name, format!("{label}>{newer}")) }).collect();
        let want = [
            ("omarchy-3.8.2.iso.part", "L-omarchy>omarchy-3.8.4.iso"),
            ("omarchy-3.8.3.iso", "L-omarchy>omarchy-3.8.4.iso"),
            ("renamed-old.img", "L-other>new-name.img"),
            ("ubuntu-24.04-desktop-amd64.iso", "L-u24>ubuntu-24.04.3-desktop-amd64.iso"),
        ];
        assert_eq!(found, want.map(|(a, b)| (a.to_string(), b.to_string())));

        let inv = stick_inventory(&d, &manifest);
        assert!(inv.installed.is_empty());
        assert_eq!(inv.others.len(), 8);
        assert!(inv.others.iter().all(|(n, s)| is_image(n) && *s == 2));
        fs::remove_dir_all(d).unwrap();
    }
}
