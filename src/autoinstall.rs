//! template/ folders and the auto_install section of ventoy/ventoy.json
//! (Python: _real_dirname .. sync_auto_install).
//!
//! Ventoy attaches an unattended-setup template to an image by exact path. The
//! Windows ISOs carry their build number in the filename, so every update would
//! silently orphan the entry written for the previous build. Templates are
//! therefore assigned by the folder they sit in: everything in /template/win11
//! applies to every Windows 11 image on the stick. The image paths are rebuilt
//! from the manifest on every run, so they follow the ISOs through updates.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::app::Args;
use crate::catalog::{Kind, catalog};
use crate::manifest::Manifest;

const TEMPLATE_ROOT: &str = "template";

/// The on-disk spelling of `wanted` inside `parent`, or None.
///
/// An exFAT stick is case-insensitive, so "Template/Win11" is found on Windows
/// and missed on Linux. Match without regard to case and hand back the real
/// spelling, so the path written into ventoy.json is one that exists.
fn real_dirname(parent: &Path, wanted: &str) -> Option<String> {
    let entries: Vec<String> = fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if entries.iter().any(|n| n == wanted) {
        return Some(wanted.to_string());
    }
    let lowered = wanted.to_lowercase();
    entries.into_iter().find(|n| n.to_lowercase() == lowered)
}

/// (stick_path, local_path) of /template/<folder>, in its real spelling.
fn template_dir(dest: &Path, folder: &str) -> (String, PathBuf) {
    let root = real_dirname(dest, TEMPLATE_ROOT).unwrap_or_else(|| TEMPLATE_ROOT.into());
    let sub = real_dirname(&dest.join(&root), folder).unwrap_or_else(|| folder.into());
    (format!("/{root}/{sub}"), dest.join(&root).join(&sub))
}

/// Stick-absolute paths of the .xml templates in /template/<folder>.
fn stick_templates(dest: &Path, folder: &str) -> Vec<String> {
    let (stick, local) = template_dir(dest, folder);
    let Ok(rd) = fs::read_dir(&local) else {
        return vec![];
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_lowercase().ends_with(".xml"))
        .collect();
    names.sort();
    names.iter().map(|n| format!("{stick}/{n}")).collect()
}

/// [(folder, image_name)] of the Windows images actually present.
/// `wins`: (catalog key, win) of every Windows catalog item, in catalog order.
fn windows_on_stick(
    dest: &Path,
    manifest: &Manifest,
    wins: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for (key, win) in wins {
        let name = manifest
            .get(*key)
            .and_then(|e| e.get("filename"))
            .and_then(Value::as_str);
        let Some(name) = name.filter(|n| !n.is_empty()) else {
            continue;
        };
        if found.iter().any(|(_, n)| n == name) || !dest.join(name).exists() {
            continue; // already listed, or recorded but not actually here
        }
        found.push((format!("win{win}"), name.to_string()));
    }
    found
}

/// Make sure a template folder exists for each Windows image on the stick.
///
/// Without this the feature is invisible: an empty stick has no /template, so
/// nothing hints that dropping an answer file there would do anything. Returns
/// the folders that exist but are still empty, so the caller can say so.
fn ensure_template_dirs(dest: &Path, found: &[(String, String)], create: bool) -> Vec<String> {
    let mut empty = Vec::new();
    for folder in found
        .iter()
        .map(|(f, _)| f.as_str())
        .collect::<BTreeSet<_>>()
    {
        let (stick, local) = template_dir(dest, folder);
        if create && !local.is_dir() && fs::create_dir_all(&local).is_err() {
            continue;
        }
        if local.is_dir() && stick_templates(dest, folder).is_empty() {
            empty.push(stick);
        }
    }
    empty
}

/// The auto_install entries the Windows ISOs on the stick should have.
fn build_auto_install(dest: &Path, found: &[(String, String)]) -> Vec<Value> {
    found
        .iter()
        .filter_map(|(folder, name)| {
            let templates = stick_templates(dest, folder);
            (!templates.is_empty())
                .then(|| json!({"image": format!("/{name}"), "template": templates}))
        })
        .collect()
}

/// Whether an auto_install entry came from here.
///
/// Decided by the templates it points at, not the image it names: an entry
/// left from a previous build names an ISO that no longer exists, and matching
/// on current names would keep exactly the stale entries this should clear.
fn is_managed(entry: &Value) -> bool {
    let Some(Value::Array(templates)) = entry.get("template") else {
        return false;
    };
    // Lowercased like the folder lookup: the stick is case-insensitive, so
    // /Template/Win11 is ours just as much as /template/win11.
    let prefix = format!("/{TEMPLATE_ROOT}/win");
    !templates.is_empty()
        && templates.iter().all(|t| {
            t.as_str()
                .is_some_and(|s| s.to_lowercase().starts_with(&prefix))
        })
}

/// Rewrite ventoy.json's auto_install to match the templates on the stick.
///
/// Only entries this program owns are touched; hand-written ones for other
/// images are carried over, and the rest of ventoy.json is preserved.
pub fn sync_auto_install(dest: &Path, manifest: &Manifest, args: &Args) {
    let wins: Vec<(&str, &str)> = catalog()
        .iter()
        .filter_map(|it| match &it.kind {
            Kind::Windows(p) => Some((it.key, p.win)),
            _ => None,
        })
        .collect();
    sync(dest, manifest, args.dry_run, &wins);
}

fn sync(dest: &Path, manifest: &Manifest, dry_run: bool, wins: &[(&str, &str)]) {
    let path = dest.join("ventoy").join("ventoy.json");
    // A file that exists but cannot be read or parsed (Notepad's ANSI or UTF-16
    // included) is the user's and stays untouched: rewriting it as
    // {"auto_install": ...} would drop the rest of their config.
    let mut config: Value = match fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(e) => {
            println!("\nventoy.json cannot be read ({e}) -- leaving it alone.");
            return;
        }
        Ok(text) => match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(v) => v,
            Err(e) => {
                println!("\nventoy.json does not parse ({e}) -- leaving it alone.");
                return;
            }
        },
    };
    let Value::Object(config) = &mut config else {
        println!("\nventoy.json is not a JSON object -- leaving it alone.");
        return;
    };

    let found = windows_on_stick(dest, manifest, wins);
    let empty = ensure_template_dirs(dest, &found, !dry_run);

    let existing = match config.get("auto_install") {
        Some(Value::Array(a)) => a.clone(),
        _ => vec![],
    };
    let (before, foreign): (Vec<Value>, Vec<Value>) = existing.into_iter().partition(is_managed);
    let wanted = build_auto_install(dest, &found);

    if before == wanted {
        // Nothing to rewrite, but an empty folder is still worth one line --
        // it is the only sign that answer files can go there at all.
        if !empty.is_empty() {
            println!(
                "\nNo unattended template in {} yet -- put an .xml there and\nit gets attached to the matching Windows images.",
                empty.join(" or ")
            );
        }
        return;
    }
    if wanted.is_empty() && foreign.is_empty() && !config.contains_key("auto_install") {
        return;
    }

    let image = |e: &Value| {
        e.get("image")
            .and_then(Value::as_str)
            .unwrap_or("None")
            .to_string()
    };
    println!("\n{0} AUTO-INSTALL TEMPLATES {0}", "-".repeat(12));
    for entry in &wanted {
        let was = before.iter().find(|e| e.get("image") == entry.get("image"));
        let mark = match was {
            Some(w) if w == entry => "unchanged",
            Some(_) => "updated",
            None => "added",
        };
        println!("  {:<9} {}", mark, image(entry));
        for template in entry["template"].as_array().into_iter().flatten() {
            println!("            {}", template.as_str().unwrap_or_default());
        }
    }
    for entry in &before {
        if !wanted.iter().any(|e| e.get("image") == entry.get("image")) {
            println!("  {:<9} {}", "dropped", image(entry));
        }
    }
    for folder in &empty {
        println!("  {:<9} {}  (put an .xml here)", "empty", folder);
    }

    if dry_run {
        println!("\n  (--dry-run -- ventoy.json not written)");
        return;
    }

    if wanted.is_empty() && foreign.is_empty() {
        config.shift_remove("auto_install");
    } else {
        config.insert(
            "auto_install".into(),
            Value::Array(foreign.into_iter().chain(wanted).collect()),
        );
    }

    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    let write = || -> std::io::Result<()> {
        fs::create_dir_all(path.parent().unwrap_or(dest))?;
        // Python wrote indent=4 in text mode (CRLF on Windows); serde_json indents
        // by 2. JSON strings never hold a raw newline, so leading spaces are pure
        // indentation: doubling them gives Python's layout.
        let nl = if cfg!(windows) { "\r\n" } else { "\n" };
        let text: String = serde_json::to_string_pretty(&*config)?
            .lines()
            .map(|l| {
                let indent = l.len() - l.trim_start_matches(' ').len();
                format!("{}{l}{nl}", " ".repeat(indent))
            })
            .collect();
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    };
    match write() {
        Ok(()) => println!("\n  ventoy.json updated."),
        Err(e) => println!("\n  could NOT write ventoy.json: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINS: &[(&str, &str)] = &[
        ("win11_de", "11"),
        ("win11_en", "11"),
        ("win10_en", "10"),
        ("win10_de", "10"),
    ];

    fn stick(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ventoy_ai_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ventoy")).unwrap();
        root
    }

    fn manifest(pairs: &[(&str, &str)]) -> Manifest {
        pairs
            .iter()
            .map(|(k, f)| (k.to_string(), json!({"filename": f})))
            .collect()
    }

    fn read_json(root: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(root.join("ventoy/ventoy.json")).unwrap()).unwrap()
    }

    /// A replaced build's entry is dropped, a hand-written one survives, the
    /// rest of the file is kept, and the on-disk spelling is written back --
    /// once with the folders spelled "Template/Win11".
    #[test]
    fn auto_install_round() {
        for (root_name, sub_name) in [("template", "win11"), ("Template", "Win11")] {
            let root = stick(root_name);
            fs::create_dir_all(root.join(root_name).join(sub_name)).unwrap();
            fs::write(root.join(root_name).join(sub_name).join("a.xml"), "").unwrap();
            let current = "Win_11_25H2_English_99999.9999.iso";
            fs::write(root.join(current), "").unwrap();
            fs::write(root.join("debian.iso"), "").unwrap();
            let original = json!({
            "theme": {"file": "/theme/x.txt"},
            "auto_install": [
                {"image": "/Win_11_25H2_English_00000.0000.iso",
                 "template": [format!("/{root_name}/{sub_name}/a.xml")]},
                {"image": "/debian.iso", "template": ["/mine/preseed.cfg"]},
            ]});
            fs::write(root.join("ventoy/ventoy.json"), original.to_string()).unwrap();

            sync(&root, &manifest(&[("win11_en", current)]), false, WINS);

            let raw = fs::read_to_string(root.join("ventoy/ventoy.json")).unwrap();
            let config = read_json(&root);
            fs::remove_dir_all(&root).unwrap();
            let entries = config["auto_install"].as_array().unwrap();
            let images: Vec<&str> = entries
                .iter()
                .map(|e| e["image"].as_str().unwrap())
                .collect();
            assert_eq!(images, ["/debian.iso", &format!("/{current}")]);
            assert_eq!(
                entries[1]["template"],
                json!([format!("/{root_name}/{sub_name}/a.xml")])
            );
            assert_eq!(config["theme"], json!({"file": "/theme/x.txt"}));
            // Key order kept, 4-space indent like Python's json.dump(indent=4).
            assert!(
                raw.starts_with("{\n    \"theme\": {\n        \"file\""),
                "{raw}"
            );
        }
    }

    /// Template folders appear only for Windows images that are there, and
    /// --dry-run creates and writes nothing.
    #[test]
    fn template_dirs() {
        let m = manifest(&[("win10_en", "Win_10.iso"), ("win11_en", "Win_11.iso")]);
        let run = |tag: &str, with_isos: bool, dry_run: bool| {
            let root = stick(tag);
            let original = "{\"theme\": {\"file\": \"/x\"}}";
            fs::write(root.join("ventoy/ventoy.json"), original).unwrap();
            if with_isos {
                fs::write(root.join("Win_10.iso"), "").unwrap();
                fs::write(root.join("Win_11.iso"), "").unwrap();
            }
            sync(&root, &m, dry_run, WINS);
            let mut dirs: Vec<String> = fs::read_dir(root.join("template"))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            dirs.sort();
            let untouched =
                fs::read_to_string(root.join("ventoy/ventoy.json")).unwrap() == original;
            fs::remove_dir_all(&root).unwrap();
            (dirs, untouched)
        };
        assert_eq!(run("isos", true, false).0, ["win10", "win11"]);
        assert_eq!(run("dry", true, true), (vec![], true));
        assert_eq!(run("none", false, false).0, Vec::<String>::new());
    }

    #[test]
    fn managed_entries() {
        assert!(is_managed(
            &json!({"image": "/x.iso", "template": ["/Template/WIN11/a.xml"]})
        ));
        assert!(!is_managed(
            &json!({"image": "/x.iso", "template": ["/template/win11/a.xml", "/mine/b.xml"]})
        ));
        assert!(!is_managed(&json!({"image": "/x.iso", "template": []})));
        assert!(!is_managed(&json!("/template/win11/a.xml")));
    }

    /// A ventoy.json saved as ANSI (not UTF-8) is left exactly as it was.
    #[test]
    fn unreadable_ventoy_json_is_left_alone() {
        let root = stick("ansi");
        fs::create_dir_all(root.join("template/win11")).unwrap();
        fs::write(root.join("template/win11/a.xml"), "").unwrap();
        fs::write(root.join("W11.iso"), "").unwrap();
        let orig: &[u8] = b"{\"theme\": {\"file\": \"/x\"}, \"menu_alias\": [{\"image\": \"/a.iso\", \"alias\": \"Men\xfc\"}]}";
        fs::write(root.join("ventoy/ventoy.json"), orig).unwrap();
        sync(&root, &manifest(&[("win11_en", "W11.iso")]), false, WINS);
        let now = fs::read(root.join("ventoy/ventoy.json")).unwrap();
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(now, orig);
    }
}
