//! Smoke-test every resolver in the catalog against the live upstreams
//! (port of tests/check_resolvers.py).
//!
//! The tool depends on ~50 foreign directory layouts. When one changes, the
//! matching resolver stops finding an ISO, and the only way to notice is to ask
//! the upstreams. Run on a schedule so a broken layout shows up here first:
//!
//!     cargo test --test resolvers -- --ignored --nocapture
//!
//! The offline version-sanity checks also run in a plain `cargo test`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use regex::Regex;
use ventoy_iso_updater::catalog::{Kind, catalog, find};
use ventoy_iso_updater::net::{remote_meta, remote_sha256_with, verkey};
use ventoy_iso_updater::resolvers::{
    FEDORA_RELEASES, FREEBSD_BASE, fedora_iso_with, freebsd_with, leap_versions, r_opensuse_leap,
};

type Entry = (
    &'static str,
    &'static str,
    fn() -> Result<ventoy_iso_updater::catalog::Resolved>,
);
/// (key, label, ok, detail)
type Outcome = (&'static str, &'static str, bool, String);

fn not_found() -> anyhow::Error {
    ureq::Error::StatusCode(404).into()
}

/// Call one resolver, never fail. Also warns when the catalog's size_mb is
/// more than 50 % off the real size, so the fit-to-stick table stays honest.
fn resolve(&(key, label, f): &Entry) -> Outcome {
    match f() {
        Ok(r) => {
            let want = find(key).map_or(0, |it| u64::from(it.size_mb) * 1_000_000);
            if let Some(real) = remote_meta(&r.url).size
                && real.abs_diff(want) > want / 2
            {
                println!(
                    "  WARN size {key:<14} is {:.1} GB, catalog says {:.1} GB",
                    real as f64 / 1e9,
                    want as f64 / 1e9
                );
            }
            (key, label, true, r.name)
        }
        Err(e) => (key, label, false, format!("{e:#}")),
    }
}

/// Every iso catalog entry must resolve to a download.
///
/// Failures are retried once, sequentially and after a pause: a mirror that
/// times out under twelve parallel requests is a hiccup, not a broken layout,
/// and a check that cries wolf is a check nobody reads.
fn check_catalog() -> bool {
    let entries: Vec<Entry> = catalog()
        .iter()
        .filter_map(|it| match it.kind {
            Kind::Iso(f) => Some((it.key, it.label, f)),
            _ => None,
        })
        .collect();
    let queue = Mutex::new(entries.iter().enumerate());
    let done = Mutex::new(Vec::new());
    thread::scope(|s| {
        for _ in 0..12 {
            s.spawn(|| {
                while let Some((i, e)) = queue.lock().unwrap().next() {
                    let r = resolve(e);
                    done.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut results = done.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);
    let mut results: Vec<Outcome> = results.into_iter().map(|(_, r)| r).collect();

    let flaky: Vec<&str> = results.iter().filter(|r| !r.2).map(|r| r.0).collect();
    if !flaky.is_empty() {
        println!(
            "  retrying {} after a pause: {}\n",
            flaky.len(),
            flaky.join(", ")
        );
        thread::sleep(Duration::from_secs(5));
        let retried: HashMap<&str, Outcome> = entries
            .iter()
            .filter(|e| flaky.contains(&e.0))
            .map(|e| (e.0, resolve(e)))
            .collect();
        for r in results.iter_mut() {
            if let Some(n) = retried.get(r.0) {
                *r = n.clone();
            }
        }
    }

    let failed = results.iter().filter(|r| !r.2).count();
    results.sort_by_key(|r| (r.2, r.0));
    for (key, label, ok, detail) in &results {
        println!(
            "  {:<4} {key:<14} {label:<24} {detail}",
            if *ok { "ok" } else { "FAIL" }
        );
    }
    println!(
        "\n{} of {} resolvers OK.",
        results.len() - failed,
        results.len()
    );
    failed == 0
}

/// openSUSE Leap must resolve to the newest release, not an older one.
///
/// Leap 16 moved its installer to a new path and filename; the old resolver
/// silently fell through to the end-of-life 15.6. Whatever the newest release
/// directory is, the resolved filename must carry that version.
fn check_leap_is_current() -> bool {
    let (newest, name) =
        match leap_versions().and_then(|v| Ok((v[0].clone(), r_opensuse_leap()?.name))) {
            Ok(x) => x,
            Err(e) => {
                println!("  FAIL leap           {e:#}");
                return false;
            }
        };
    let found = Regex::new(r"(\d+\.\d+)")
        .unwrap()
        .captures(&name)
        .map(|c| c[1].to_string());
    if found.as_deref() == Some(newest.as_str()) {
        println!("  ok   leap           newest release {newest} -> {name}");
        return true;
    }
    println!(
        "  FAIL leap           newest release is {newest} but resolved {name} -- the layout for {newest} probably changed"
    );
    false
}

/// A point release must sort above its base release: a flattened verkey let
/// the "64" of amd64 outrank the ".1" of 26.04.1.
fn check_point_releases() -> bool {
    let pairs = [
        (
            "ubuntu-26.04-desktop-amd64.iso",
            "ubuntu-26.04.1-desktop-amd64.iso",
        ),
        (
            "ubuntu-24.04.5-desktop-amd64.iso",
            "ubuntu-24.04.5.1-desktop-amd64.iso",
        ),
        (
            "linuxmint-22-cinnamon-64bit.iso",
            "linuxmint-22.1-cinnamon-64bit.iso",
        ),
    ];
    let bad: Vec<_> = pairs
        .iter()
        .filter(|(old, new)| verkey(new) <= verkey(old))
        .collect();
    for (old, new) in &bad {
        println!("  FAIL verkey         {new} does not sort above {old}");
    }
    if bad.is_empty() {
        println!("  ok   verkey         point releases sort above their base");
    }
    bad.is_empty()
}

/// A Fedora release staged before release day (listed, every subdir 404) must
/// not break the resolver; the previous release is still current then.
fn check_fedora_staged_release() -> bool {
    let kde = format!("{FEDORA_RELEASES}44/KDE/x86_64/iso/");
    let get = |u: &str| -> Result<String> {
        if u == FEDORA_RELEASES {
            Ok(r#"<a href="44/"> <a href="45/"> <a href="test/">"#.into())
        } else if u == kde {
            Ok(r#"<a href="Fedora-KDE-Desktop-Live-44-1.7.x86_64.iso">"#.into())
        } else {
            Err(not_found())
        }
    };
    let name = match fedora_iso_with(
        &get,
        "KDE",
        r#"href="(Fedora-KDE-Desktop-Live-\d+-[\d.]+\.x86_64\.iso)""#,
    ) {
        Ok((_, n)) => n,
        Err(e) => format!("{e:#}"),
    };
    if name == "Fedora-KDE-Desktop-Live-44-1.7.x86_64.iso" {
        println!("  ok   fedora         staged 45 skipped -> {name}");
        return true;
    }
    println!("  FAIL fedora         staged 45 should fall back to 44, got {name}");
    false
}

/// A FreeBSD release directory holding only BETA images must be skipped.
fn check_freebsd_prerelease() -> bool {
    let get = |u: &str| -> Result<String> {
        Ok(match u.strip_prefix(FREEBSD_BASE).unwrap_or(u) {
            "" => r#"<a href="15.1/"> <a href="15.2/">"#,
            "15.2/" => r#"<a href="FreeBSD-15.2-BETA1-amd64-disc1.iso">"#,
            "15.1/" => r#"<a href="FreeBSD-15.1-RELEASE-amd64-disc1.iso">"#,
            _ => return Err(not_found()),
        }
        .into())
    };
    let name = match freebsd_with(&get) {
        Ok((_, n)) => n,
        Err(e) => format!("{e:#}"),
    };
    if name == "FreeBSD-15.1-RELEASE-amd64-disc1.iso" {
        println!("  ok   freebsd        BETA-only 15.2 skipped -> {name}");
        return true;
    }
    println!("  FAIL freebsd        BETA-only 15.2 should fall back to 15.1, got {name}");
    false
}

/// A hex string inside an HTML page is not a published checksum (SourceForge
/// answers "<image>.sha256" with its file browser), but a real one-line sums
/// file counts whatever filename it mentions.
fn check_sums_ignore_html() -> bool {
    let digest = "ab".repeat(32);
    let html = format!(
        r#"<html><img src="//a.example/icon?{}?w=48">"#,
        "cd".repeat(32)
    );
    let get = |u: &str| -> Result<String> {
        match u {
            "https://example.invalid/a.iso.sha256" => Ok(html.clone()),
            "https://example.invalid/b.iso.sha256" => {
                Ok(format!("{digest}  upstream-build-name.iso\n"))
            }
            _ => Err(not_found()),
        }
    };
    let from_html = remote_sha256_with(&get, "https://example.invalid/a.iso", "a.iso");
    let from_sums = remote_sha256_with(&get, "https://example.invalid/b.iso", "b.iso");
    if from_html.is_none() && from_sums.as_deref() == Some(digest.as_str()) {
        println!("  ok   sums           HTML page ignored, one-line sums file read");
        return true;
    }
    println!(
        "  FAIL sums           HTML page gave {from_html:?}, one-line sums file gave {from_sums:?}"
    );
    false
}

/// Every entry needs a unique key and a description line for the picker.
fn check_catalog_integrity() -> bool {
    let keys: Vec<&str> = catalog().iter().map(|it| it.key).collect();
    let mut ok = true;
    let mut dups: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|k| keys.iter().filter(|x| *x == k).count() > 1)
        .collect();
    dups.sort();
    dups.dedup();
    if !dups.is_empty() {
        println!("  FAIL duplicate catalog keys: {}", dups.join(", "));
        ok = false;
    }
    let undescribed: Vec<&str> = catalog()
        .iter()
        .filter(|it| it.desc.is_empty())
        .map(|it| it.key)
        .collect();
    if !undescribed.is_empty() {
        println!("  FAIL no description: {}", undescribed.join(", "));
        ok = false;
    }
    if ok {
        println!(
            "  ok   {} catalog entries, all keyed and described.",
            keys.len()
        );
    }
    ok
}

/// Version-sanity checks that need no network.
fn offline_checks() -> bool {
    // Run them all, then combine, so every failure gets printed.
    [
        check_point_releases(),
        check_fedora_staged_release(),
        check_freebsd_prerelease(),
        check_sums_ignore_html(),
    ]
    .iter()
    .all(|ok| *ok)
}

#[test]
fn offline_sanity() {
    assert!(check_catalog_integrity() && offline_checks());
}

#[test]
#[ignore = "network: asks every upstream"]
fn resolvers() {
    println!("=== catalog integrity ===");
    let integrity_ok = check_catalog_integrity();
    println!("\n=== catalog resolvers ===");
    let catalog_ok = check_catalog();
    println!("\n=== version sanity ===");
    let leap_ok = check_leap_is_current();
    let offline_ok = offline_checks();
    if !(integrity_ok && catalog_ok && leap_ok && offline_ok) {
        println!("\nSomething upstream changed. Fix the resolver, then rerun.");
        panic!("resolver check failed");
    }
}
