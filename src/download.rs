//! Downloader, progress line, moves, MemTest86, fetch_tracked
//! (Python: _progress .. download_memtest, fetch_tracked).

use std::path::Path;

use anyhow::Result;

use crate::app::Args;
use crate::manifest::{Check, Manifest};
use crate::Summary;

/// url -> dest via dest.part with resume, live speed, optional SHA-256 check.
pub fn download(_url: &str, _dest: &Path, _sha256: Option<&str>) -> Result<()> { todo!() }
/// Move a file, with a progress line whenever it has to be copied.
pub fn move_with_progress(_src: &Path, _dst: &Path) -> Result<()> { todo!() }
/// Download the MemTest86 zip and extract memtest86-usb.img into dest_dir.
pub fn download_memtest(_dest_dir: &Path) -> Result<()> { todo!() }

/// Update-check -> (mirror race) -> checksum -> download -> record.
#[allow(clippy::too_many_arguments)]
pub fn fetch_tracked(
    _dest: &Path, _manifest: &mut Manifest, _key: &str, _label: &str, _url: &str,
    _name: &str, _mirrors: &[String], _args: &Args, _summary: &mut Summary,
    _checked: Option<Check>,
) { todo!() }
