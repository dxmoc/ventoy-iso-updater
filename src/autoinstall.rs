//! template/ folders and the auto_install section of ventoy/ventoy.json
//! (Python: _real_dirname .. sync_auto_install).

use std::path::Path;

use crate::app::Args;
use crate::manifest::Manifest;

pub fn sync_auto_install(_dest: &Path, _manifest: &Manifest, _args: &Args) { todo!() }
