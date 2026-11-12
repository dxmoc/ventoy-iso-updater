//! Windows ISOs: massgrave hand-off, Fido, UUP dump
//! (Python: detect_windows_builds .. _massgrave_handoff).

use std::path::Path;

use crate::app::Args;
use crate::catalog::Item;
use crate::manifest::Manifest;
use crate::Summary;

pub fn is_admin() -> bool { todo!() }
/// Relaunch elevated (UAC); true if the elevated copy took over.
pub fn relaunch_as_admin() -> bool { todo!() }
pub fn windows_needs_build(_dest: &Path, _manifest: &Manifest, _args: &Args, _items: &[&'static Item]) -> bool { todo!() }
pub fn process_windows(_dest: &Path, _manifest: &mut Manifest, _args: &Args, _items: &[&'static Item], _summary: &mut Summary) { todo!() }
