//! ventoy_versions.json and the update decision (Python: load_manifest ..
//! check_for_update). Kept as raw JSON so unknown fields survive a rewrite.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::net::{self, Meta};
use crate::{MANIFEST_NAME, REPO, VERSION};

/// key -> {"filename", "url", "size", "mtime", "history"?}
pub type Manifest = serde_json::Map<String, serde_json::Value>;

/// Result of check_update(): needs download?, why, remote meta if fetched.
#[derive(Clone, Debug)]
pub struct Check {
    pub need: bool,
    pub reason: String,
    pub meta: Option<Meta>,
}

/// Former filenames remembered per item.
const HISTORY_KEEP: usize = 5;

pub fn load(dest: &Path) -> Manifest {
    fs::read_to_string(dest.join(MANIFEST_NAME))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            if let Value::Object(m) = v {
                Some(m)
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Byte-compatible with Python's json.dump(indent=2, ensure_ascii=False,
/// sort_keys=True); written to a .tmp and renamed so a pulled stick never
/// holds half a manifest.
pub fn save(dest: &Path, manifest: &Manifest) -> std::io::Result<()> {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut m: Manifest = m.iter().map(|(k, v)| (k.clone(), sorted(v))).collect();
                m.sort_keys();
                Value::Object(m)
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    let path = dest.join(MANIFEST_NAME);
    let tmp = dest.join(format!("{MANIFEST_NAME}.tmp"));
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&sorted(&Value::Object(manifest.clone())))?,
    )?;
    fs::rename(tmp, path)
}

pub fn entry_filename<'a>(manifest: &'a Manifest, key: &str) -> Option<&'a str> {
    manifest
        .get(key)?
        .get("filename")?
        .as_str()
        .filter(|s| !s.is_empty())
}

/// Python int() of a manifest value; None also for 0, which Python treats as unset.
fn as_size(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    v.as_u64()
        .or_else(|| v.as_str()?.trim().parse().ok())
        .filter(|&n| n > 0)
}

/// Decide whether `name` needs (re)downloading. Three cases, and in two of
/// them a file that is already correct is adopted rather than fetched again:
///   * a manifest record under the same name -- the fast rolling-release check
///   * a record under a different name, yet the new name is already on disk:
///     a download that completed but was never recorded
///   * no record at all, e.g. an ISO put there by hand
///
/// In the last two the local size is compared against the remote one; a match
/// adopts the file, a mismatch redownloads, and an unreadable remote size
/// leaves the file alone rather than destroying it on a guess.
pub fn check_update(
    dest: &Path,
    entry: Option<&serde_json::Value>,
    url: &str,
    name: &str,
) -> Check {
    check_update_with(dest, entry, name, || net::remote_meta(url))
}

fn check_update_with(
    dest: &Path,
    entry: Option<&Value>,
    name: &str,
    remote: impl FnOnce() -> Meta,
) -> Check {
    let check = |need: bool, reason: String, meta: Meta| Check {
        need,
        reason,
        meta: Some(meta),
    };
    let Ok(md) = fs::metadata(dest.join(name)) else {
        return Check {
            need: true,
            reason: "not on stick".into(),
            meta: None,
        };
    };
    let local = md.len();
    // Python truthiness: an empty record counts as no record.
    let entry = entry.and_then(Value::as_object).filter(|e| !e.is_empty());

    if let Some(e) = entry {
        let recorded = e.get("filename").and_then(Value::as_str);
        let meta = remote();
        let size = meta.size.filter(|&s| s > 0);
        if recorded == Some(name) {
            if let (Some(s), Some(old)) = (size, as_size(e.get("size")))
                && s != old
            {
                return check(true, "remote size changed".into(), meta);
            }
            // Last-Modified alone is not trustworthy: mirrors and CDNs hand out
            // different timestamps for byte-identical files, which would trigger
            // pointless multi-GB re-downloads. Only trust it when there is no size.
            let old_mtime = e
                .get("mtime")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            if size.is_none()
                && let (Some(m), Some(old)) =
                    (meta.mtime.as_deref().filter(|s| !s.is_empty()), old_mtime)
                && m != old
            {
                return check(true, "remote updated".into(), meta);
            }
            return check(false, "up to date".into(), meta);
        }
        // The file under the *new* name is already here, so a previous run
        // downloaded it and never got to record it (stick pulled, Ctrl+C, write
        // error). Adopt it if it matches the remote size.
        let was = recorded.unwrap_or("None");
        if size == Some(local) {
            return check(false, format!("present, adopted (was {was})"), meta);
        }
        return check(true, format!("new version {was} -> {name}"), meta);
    }

    // Present on the stick but not in the manifest -> adopt or refresh by size.
    let meta = remote();
    match meta.size.filter(|&s| s > 0) {
        Some(s) if s == local => check(false, "present, adopted".into(), meta),
        Some(_) => check(true, "size mismatch -> redownload".into(), meta),
        None => check(false, "present (size unverifiable)".into(), meta),
    }
}

pub fn record_version(
    dest: &Path,
    manifest: &mut Manifest,
    key: &str,
    url: &str,
    name: &str,
    meta: Option<&Meta>,
) {
    if let Err(e) = try_record_version(dest, manifest, key, url, name, meta) {
        println!("    could not write {MANIFEST_NAME}: {e}");
    }
}

pub(crate) fn try_record_version(
    dest: &Path,
    manifest: &mut Manifest,
    key: &str,
    url: &str,
    name: &str,
    meta: Option<&Meta>,
) -> std::io::Result<()> {
    let size = meta
        .and_then(|m| m.size.filter(|&s| s > 0))
        .or_else(|| fs::metadata(dest.join(name)).ok().map(|m| m.len()));
    let mtime = meta.and_then(|m| m.mtime.clone());
    // Remember the filenames this item used to have. Version bumps that also
    // change the naming scheme are otherwise untraceable, and the leftover file
    // would sit on the stick forever -- see cleanup::find_superseded().
    let prev = manifest.get(key);
    let mut history: Vec<Value> = prev
        .and_then(|p| p.get("history"))
        .and_then(Value::as_array)
        .map(|h| {
            h.iter()
                .filter(|h| h.as_str() != Some(name))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if let Some(old) = entry_filename(manifest, key)
        && old != name
        && !history.iter().any(|h| h.as_str() == Some(old))
    {
        history.push(Value::from(old));
    }
    let mut rec = serde_json::json!({"filename": name, "url": url, "size": size, "mtime": mtime});
    if !history.is_empty() {
        rec["history"] =
            Value::Array(history.split_off(history.len().saturating_sub(HISTORY_KEEP)));
    }
    manifest.insert(key.to_string(), rec);
    save(dest, manifest)
}

/// Startup notice when a newer release is out; silent on any error.
/// This must never stand between the user and the run itself.
pub fn check_for_update() {
    let Some(data) = net::http_get_timeout(
        &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        5,
    )
    .ok()
    .and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
        return;
    };
    let Some(tag) = data.get("tag_name").and_then(Value::as_str) else {
        return;
    };
    if net::verkey(tag) > net::verkey(VERSION) {
        println!("A newer version is available: {tag} (you have {VERSION})");
        let fallback = format!("https://github.com/{REPO}/releases/latest");
        println!(
            "  {}\n",
            data.get("html_url")
                .and_then(Value::as_str)
                .unwrap_or(&fallback)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("vi_manifest_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_matches_python_json_dump() {
        let d = tmpdir("save");
        let m: Manifest = serde_json::from_str(
            r#"{"z": {"url": "u", "filename": "ä.iso", "size": 5, "mtime": null, "history": ["b", "a"]}, "a": {}}"#,
        )
        .unwrap();
        save(&d, &m).unwrap();
        let text = fs::read_to_string(d.join(MANIFEST_NAME)).unwrap();
        // json.dumps(m, indent=2, ensure_ascii=False, sort_keys=True)
        let python = "{\n  \"a\": {},\n  \"z\": {\n    \"filename\": \"ä.iso\",\n    \"history\": [\n      \"b\",\n      \"a\"\n    ],\n    \"mtime\": null,\n    \"size\": 5,\n    \"url\": \"u\"\n  }\n}";
        assert_eq!(text, python);
        assert_eq!(load(&d), m);
        assert!(!d.join(format!("{MANIFEST_NAME}.tmp")).exists());
        fs::write(d.join(MANIFEST_NAME), "not json").unwrap();
        assert!(load(&d).is_empty());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn record_version_keeps_history() {
        let d = tmpdir("record");
        let mut m = Manifest::new();
        fs::write(d.join("x-1.iso"), b"abc").unwrap();
        for v in 1..=8 {
            let meta = Meta {
                size: Some(10),
                mtime: None,
            };
            try_record_version(&d, &mut m, "x", "u", &format!("x-{v}.iso"), Some(&meta)).unwrap();
        }
        let e = &m["x"];
        assert_eq!(e["filename"], "x-8.iso");
        assert_eq!(e["size"], 10);
        assert_eq!(e["mtime"], Value::Null);
        assert_eq!(
            e["history"],
            serde_json::json!(["x-3.iso", "x-4.iso", "x-5.iso", "x-6.iso", "x-7.iso"])
        );
        // Going back to a name in the history drops it from there; no meta -> local size.
        try_record_version(&d, &mut m, "x", "u", "x-1.iso", None).unwrap();
        assert_eq!(m["x"]["size"], 3);
        assert_eq!(load(&d), m);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn check_update_cases() {
        let d = tmpdir("check");
        fs::write(d.join("a-2.iso"), b"12345").unwrap();
        let meta = |size: Option<u64>, mtime: Option<&str>| Meta {
            size,
            mtime: mtime.map(str::to_owned),
        };
        let run = |entry: Value, name: &str, m: Meta| {
            let c = check_update_with(&d, Some(&entry), name, || m);
            (c.need, c.reason)
        };
        let r = |need: bool, s: &str| (need, s.to_string());

        assert_eq!(
            check_update_with(&d, None, "missing.iso", || unreachable!()).reason,
            "not on stick"
        );
        let same = serde_json::json!({"filename": "a-2.iso", "size": 5, "mtime": "old"});
        assert_eq!(
            run(same.clone(), "a-2.iso", meta(Some(5), Some("new"))),
            r(false, "up to date")
        );
        assert_eq!(
            run(same.clone(), "a-2.iso", meta(Some(6), None)),
            r(true, "remote size changed")
        );
        assert_eq!(
            run(same.clone(), "a-2.iso", meta(None, Some("new"))),
            r(true, "remote updated")
        );
        assert_eq!(
            run(
                serde_json::json!({"filename": "a-2.iso", "size": "5"}),
                "a-2.iso",
                meta(Some(6), None)
            ),
            r(true, "remote size changed")
        );
        let renamed = serde_json::json!({"filename": "a-1.iso"});
        assert_eq!(
            run(renamed.clone(), "a-2.iso", meta(Some(5), None)),
            r(false, "present, adopted (was a-1.iso)")
        );
        assert_eq!(
            run(renamed, "a-2.iso", meta(None, None)),
            r(true, "new version a-1.iso -> a-2.iso")
        );
        assert_eq!(
            run(serde_json::json!({}), "a-2.iso", meta(Some(5), None)),
            r(false, "present, adopted")
        );
        assert_eq!(
            run(Value::Null, "a-2.iso", meta(Some(9), None)),
            r(true, "size mismatch -> redownload")
        );
        assert_eq!(
            run(Value::Null, "a-2.iso", meta(None, None)),
            r(false, "present (size unverifiable)")
        );
        fs::remove_dir_all(&d).unwrap();
    }
}
