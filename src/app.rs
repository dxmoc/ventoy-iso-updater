//! CLI and the top-level run (Python: main()).

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Preset { Standard, Advanced, Everything, Custom }

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Cleanup { Ask, Yes, No }

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum WinMethod { Massgrave, Uup, Fido }

#[derive(Parser, Debug, Clone)]
#[command(name = "ventoy-iso-updater", version, about = "Update Ventoy ISOs to newest.")]
pub struct Args {
    /// target folder / Ventoy drive (if omitted, you get an interactive picker)
    #[arg(long)]
    pub dest: Option<String>,
    /// re-download every ISO, ignoring the version manifest
    #[arg(long)]
    pub force: bool,
    /// only report what would be downloaded, deleted or left alone; writes nothing
    #[arg(long)]
    pub dry_run: bool,
    /// skip the SHA-256 check against the checksum file published next to the image
    #[arg(long)]
    pub no_verify: bool,
    /// don't ask GitHub at startup whether a newer release is out
    #[arg(long)]
    pub no_update_check: bool,
    /// skip the fastest-mirror speed test; use the default source directly
    #[arg(long)]
    pub no_mirror_test: bool,
    /// skip the interactive menu and use this selection
    #[arg(long, value_enum)]
    pub preset: Option<Preset>,
    /// what to do with superseded ISOs: ask (default), yes (delete), no (only list)
    #[arg(long, value_enum, default_value = "ask")]
    pub cleanup: Cleanup,
    /// Windows ISO method, skipping the prompt (massgrave is the default)
    #[arg(long, value_enum)]
    pub windows: Option<WinMethod>,
}

/// Whole program; returns the exit code.
pub fn run() -> i32 {
    todo!("port main()")
}
