//! Interactive pickers (Python: _quit .. choose_windows).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::app::{Args, Preset};
use crate::catalog::Item;
use crate::manifest::Manifest;

/// Pick the target drive interactively.
pub fn choose_dest() -> PathBuf { todo!() }
pub fn choose_selection(_preset: Option<Preset>, _dest: &Path, _manifest: &Manifest) -> Vec<&'static Item> { todo!() }
/// Opt-in prompt for Windows ISOs; also settles args.windows if still None.
pub fn choose_windows(_already: &HashSet<&str>, _args: &mut Args) -> Vec<&'static Item> { todo!() }
