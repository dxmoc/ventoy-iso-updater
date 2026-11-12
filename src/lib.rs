//! Keeps the ISOs on a Ventoy stick up to date. Port of the former
//! `update_ventoy_isos.py`; module boundaries follow its sections.

pub mod app;
pub mod autoinstall;
pub mod catalog;
pub mod cleanup;
pub mod download;
pub mod drives;
pub mod manifest;
pub mod net;
pub mod resolvers;
pub mod style;
pub mod ui;
pub mod windows;

/// Per-item outcome lines for the final summary: (name, status). Status starts
/// with OK / SKIP / FAIL / MANUAL / WOULD ..., which the summary counts.
pub type Summary = Vec<(String, String)>;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPO: &str = "dxmoc/ventoy-iso-updater";
/// Manifest of installed versions, written next to the ISOs on the stick.
pub const MANIFEST_NAME: &str = "ventoy_versions.json";
