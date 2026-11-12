//! ventoy_versions.json and the update decision (Python: load_manifest ..
//! check_for_update). Kept as raw JSON so unknown fields survive a rewrite.

use std::path::Path;

use crate::net::Meta;

/// key -> {"filename", "url", "size", "mtime", "history"?}
pub type Manifest = serde_json::Map<String, serde_json::Value>;

/// Result of check_update(): needs download?, why, remote meta if fetched.
#[derive(Clone, Debug)]
pub struct Check {
    pub need: bool,
    pub reason: String,
    pub meta: Option<Meta>,
}

pub fn load(_dest: &Path) -> Manifest { todo!() }
pub fn save(_dest: &Path, _manifest: &Manifest) -> std::io::Result<()> { todo!() }
pub fn entry_filename<'a>(_manifest: &'a Manifest, _key: &str) -> Option<&'a str> { todo!() }
pub fn check_update(_dest: &Path, _entry: Option<&serde_json::Value>, _url: &str, _name: &str) -> Check { todo!() }
pub fn record_version(_dest: &Path, _manifest: &mut Manifest, _key: &str, _url: &str, _name: &str, _meta: Option<&Meta>) { todo!() }
/// Startup notice when a newer release is out; silent on any error.
pub fn check_for_update() { todo!() }
