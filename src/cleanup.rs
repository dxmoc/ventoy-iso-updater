//! Superseded-version sweep and stick inventory
//! (Python: _family_regex .. print_inventory).

use std::collections::BTreeMap;
use std::path::Path;

use crate::app::Cleanup;
use crate::catalog::Resolved;
use crate::manifest::Manifest;
use crate::Summary;

/// key -> (label, filename that should be on the stick).
pub type Current = BTreeMap<String, (String, String)>;

/// What is on the stick: catalog key -> (filename, size), plus unaccounted images.
#[derive(Debug, Default)]
pub struct Inventory {
    pub installed: BTreeMap<String, (String, u64)>,
    pub others: Vec<(String, u64)>,
}

/// `resolved`: (key, label, Resolved) of this run; `extra`: (key, label, filename).
pub fn current_files(_manifest: &Manifest, _resolved: &[(String, String, Resolved)], _extra: &[(&str, &str, &str)]) -> Current { todo!() }
pub fn cleanup_superseded(_dest: &Path, _manifest: &mut Manifest, _current: &Current, _mode: Cleanup, _summary: &mut Summary) { todo!() }
pub fn stick_inventory(_dest: &Path, _manifest: &Manifest) -> Inventory { todo!() }
/// `numbers`: key -> catalog number shown in front of each line.
pub fn print_inventory(_dest: &Path, _inv: &Inventory, _numbers: Option<&BTreeMap<String, usize>>) { todo!() }
