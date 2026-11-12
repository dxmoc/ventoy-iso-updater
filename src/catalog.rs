//! The catalog of downloadable items (Python: CATALOG, DESCRIPTIONS, TIERS).

use crate::app::Preset;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier { Standard, Advanced, Extra, Windows }

/// What a resolver found: download URL, target filename, mirror candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub url: String,
    pub name: String,
    pub mirrors: Vec<String>,
}

pub type Resolver = fn() -> anyhow::Result<Resolved>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WinSpec {
    pub win: &'static str,
    pub rel: &'static str,
    pub lang: &'static str,
    pub page: &'static str,
    pub pick: &'static str,
    /// Filename template, `{}` is replaced by the build number.
    pub tmpl: &'static str,
    pub server: bool,
}

#[derive(Clone, Copy)]
pub enum Kind {
    Iso(Resolver),
    Memtest,
    Windows(WinSpec),
}

#[derive(Clone, Copy)]
pub struct Item {
    pub key: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub tier: Tier,
    pub kind: Kind,
    /// One-line "what is this" for the picker (Python: DESCRIPTIONS).
    pub desc: &'static str,
}

/// All items, in picker order.
pub fn catalog() -> &'static [Item] { todo!() }

pub fn find(key: &str) -> Option<&'static Item> {
    catalog().iter().find(|it| it.key == key)
}

/// Items of a preset; Windows is in none (Custom returns nothing).
pub fn preset_items(_preset: Preset) -> Vec<&'static Item> { todo!() }
