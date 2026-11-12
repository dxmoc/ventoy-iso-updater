//! Version resolvers: one `r_*` fn per catalog entry (Python: r_arch .. r_freebsd).

use std::collections::BTreeSet;
use std::hash::{BuildHasher, Hasher};

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde_json::Value;

use crate::catalog::Resolved;
use crate::net::{
    self, find_all, http_get, http_get_cached_timeout, latest_in_listing, pick_latest, verkey,
};

/// A page fetcher, so tests can feed fake listings to the fallback logic.
pub type Get<'a> = &'a dyn Fn(&str) -> Result<String>;

/// Third-party mirrors per speed test, on top of the primary.
const MIRROR_RACE_SIZE: usize = 5;

fn plain(found: Result<(String, String)>) -> Result<Resolved> {
    found.map(Into::into)
}

fn with_mirrors((url, name): (String, String), mirrors: Vec<String>) -> Resolved {
    Resolved { url, name, mirrors }
}

/// The first of the highest-versioned items (Python: max(xs, key=verkey)).
fn max_ver(items: Vec<String>) -> Option<String> {
    items.into_iter().rev().max_by_key(|s| verkey(s))
}

/// Newest first by verkey.
fn sorted_desc(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = items
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    v.sort_by_key(|s| std::cmp::Reverse(verkey(s)));
    v
}

/// Up to `n` random elements, in random order (Python: random.sample).
fn sample(mut v: Vec<String>, n: usize) -> Vec<String> {
    let mut x = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
        | 1;
    let n = n.min(v.len());
    for i in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17; // xorshift64
        let j = i + (x % (v.len() - i) as u64) as usize;
        v.swap(i, j);
    }
    v.truncate(n);
    v
}

/// Best-scored HTTPS Arch mirrors (lower score = healthier) as full ISO URLs.
fn arch_mirrors(limit: usize) -> Vec<String> {
    let Ok(data) = http_get("https://archlinux.org/mirrors/status/json/")
        .and_then(|t| Ok(serde_json::from_str::<Value>(&t)?))
    else {
        return Vec::new();
    };
    let truthy = |v: &Value| match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Null => false,
        _ => true,
    };
    let mut good: Vec<(f64, String)> = data["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| {
            m["protocol"] == "https"
                && truthy(&m["active"])
                && m["completion_pct"].as_f64().unwrap_or(0.0) >= 1.0
                && truthy(&m["score"])
        })
        .filter_map(|m| Some((m["score"].as_f64()?, m["url"].as_str()?.to_string())))
        .collect();
    good.sort_by(|a, b| a.0.total_cmp(&b.0));
    good.into_iter()
        .take(limit)
        .map(|(_, u)| {
            format!(
                "{}/iso/latest/archlinux-x86_64.iso",
                u.trim_end_matches('/')
            )
        })
        .collect()
}

/// Two-letter country of this machine from the OS settings, or None.
///
/// Picks the Ubuntu mirrors to race without asking a geo-IP service. Windows
/// has a region setting of its own; elsewhere the locale's territory
/// (de_DE.UTF-8 -> DE) is the best offline hint there is.
pub fn country_code() -> Option<String> {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetUserDefaultGeoName(geo_name: *mut u16, count: i32) -> i32;
        }
        let mut buf = [0u16; 16];
        // SAFETY: buf is valid for its full length, which is passed along.
        let n = unsafe { GetUserDefaultGeoName(buf.as_mut_ptr(), buf.len() as i32) };
        if n > 0 {
            let s = String::from_utf16_lossy(
                &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())],
            );
            if s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase()) {
                return Some(s);
            }
        }
    }
    let re = Regex::new(r"^[a-z]{2,3}_([A-Z]{2})").unwrap();
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| Some(re.captures(&std::env::var(v).ok()?)?[1].to_string()))
}

const UBUNTU_RELEASES: &str = "https://releases.ubuntu.com/";

/// Official Ubuntu release mirrors in this machine's country, as ISO URLs.
///
/// releases.ubuntu.com is a single origin and delivered about half of what a
/// local mirror did in testing. Launchpad lists the mirrors per country; a
/// random sample spreads the load. No country, no list.
fn ubuntu_mirrors(primary: &str) -> Vec<String> {
    let Some(cc) = country_code() else {
        return Vec::new();
    };
    let Some(rest) = primary.strip_prefix(UBUNTU_RELEASES) else {
        return Vec::new();
    };
    let url = format!(
        "https://api.launchpad.net/devel/ubuntu?ws.op=getBestMirrorsForCountry\
         &country=https://api.launchpad.net/devel/%2Bcountries/{cc}&mirror_type=CD%20Image"
    );
    let Ok(data) =
        http_get_cached_timeout(&url, 15).and_then(|t| Ok(serde_json::from_str::<Value>(&t)?))
    else {
        return Vec::new();
    };
    // Launchpad sometimes lists the primary itself; it races anyway.
    let bases: Vec<String> = data["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["https_base_url"].as_str().filter(|s| !s.is_empty()))
        .filter(|b| net::host_of(b) != net::host_of(UBUNTU_RELEASES))
        .map(String::from)
        .collect();
    sample(bases, MIRROR_RACE_SIZE)
        .into_iter()
        .map(|b| format!("{}/{rest}", b.trim_end_matches('/')))
        .collect()
}

/// Newest point release of one Ubuntu series and edition, plus mirrors.
fn ubuntu_release(series: &str, kind: &str) -> Result<Resolved> {
    let found = latest_in_listing(
        &format!("{UBUNTU_RELEASES}{series}/"),
        &format!(
            r#"href="(ubuntu-{}(?:\.\d+)*-{kind}-amd64\.iso)""#,
            regex::escape(series)
        ),
    )?;
    let mirrors = ubuntu_mirrors(&found.0);
    Ok(with_mirrors(found, mirrors))
}

pub fn r_arch() -> Result<Resolved> {
    // /latest/ always points to the newest snapshot. Primary is the geo CDN;
    // extra candidates come from the mirror-status list for the speed test.
    let primary = "https://geo.mirror.pkgbuild.com/iso/latest/archlinux-x86_64.iso";
    Ok(with_mirrors(
        (primary.into(), "archlinux-x86_64.iso".into()),
        arch_mirrors(6),
    ))
}

pub fn r_debian() -> Result<Resolved> {
    plain(latest_in_listing(
        "https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/",
        r#"href="(debian-[\d.]+-amd64-netinst\.iso)""#,
    ))
}

pub const FEDORA_RELEASES: &str = "https://mirrors.kernel.org/fedora/releases/";

/// Newest Fedora ISO of one edition, with the listing fetcher passed in.
///
/// dl.fedoraproject.org is behind an anti-bot challenge, so the release list
/// comes from an open mirror. A new release is synced days early with its
/// subdirectories locked: the number is listed, everything below answers 404
/// (or 403) until release day, and the previous release is still current.
pub fn fedora_iso_with(get: Get, edition: &str, iso_regex: &str) -> Result<(String, String)> {
    let nums: BTreeSet<u64> = find_all(r#"href="(\d+)/""#, &get(FEDORA_RELEASES)?)?
        .iter()
        .filter_map(|n| n.parse().ok())
        .collect();
    let tries: Vec<u64> = nums.into_iter().rev().take(2).collect();
    let Some(&last) = tries.last() else {
        bail!("could not list Fedora releases")
    };
    for &num in &tries {
        let iso_base = format!("{FEDORA_RELEASES}{num}/{edition}/x86_64/iso/");
        match get(&iso_base).and_then(|html| pick_latest(&iso_base, &html, iso_regex)) {
            Ok(found) => return Ok(found),
            Err(e) => match net::http_status(&e) {
                // shortcut: a moved edition path also answers 404 and lands here;
                // the warning keeps it visible. Tighten if that ever happens.
                Some(code @ (403 | 404)) if num != last => {
                    crate::style::note(format!(
                        "Fedora {num} {edition} not public yet (HTTP {code}), using {last}"
                    ));
                }
                _ => return Err(e),
            },
        }
    }
    unreachable!("the last try returns or raises")
}

fn fedora_iso(edition: &str, iso_regex: &str) -> Result<Resolved> {
    let found = fedora_iso_with(&net::http_get_cached, edition, iso_regex)?;
    let mirrors = fedora_mirrors(&found.0);
    Ok(with_mirrors(found, mirrors))
}

/// Fedora mirrors near this machine, as full ISO URLs for the speed test.
///
/// kernel.org is only the listing source; from Germany it served the ISOs at
/// ~120 KB/s. MirrorManager geo-locates the caller and returns mirrors
/// best-first, so no country is needed here.
fn fedora_mirrors(primary: &str) -> Vec<String> {
    let Some(rest) = primary.strip_prefix(FEDORA_RELEASES) else {
        return Vec::new();
    };
    let (path, name) = rest.rsplit_once('/').unwrap_or(("", rest));
    let url = format!(
        "https://mirrors.fedoraproject.org/mirrorlist?path=pub/fedora/linux/releases/{path}/"
    );
    let Ok(text) = http_get_cached_timeout(&url, 15) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| l.starts_with("https://"))
        .take(MIRROR_RACE_SIZE)
        .map(|b| format!("{}/{name}", b.trim().trim_end_matches('/')))
        .collect()
}

pub fn r_fedora_kde() -> Result<Resolved> {
    fedora_iso(
        "KDE",
        r#"href="(Fedora-KDE-Desktop-Live-\d+-[\d.]+\.x86_64\.iso)""#,
    )
}

pub fn r_ubuntu_server_2404() -> Result<Resolved> {
    ubuntu_release("24.04", "live-server")
}
pub fn r_ubuntu_server_2604() -> Result<Resolved> {
    ubuntu_release("26.04", "live-server")
}
pub fn r_ubuntu_desktop_2604() -> Result<Resolved> {
    ubuntu_release("26.04", "desktop")
}
pub fn r_ubuntu_desktop_2404() -> Result<Resolved> {
    ubuntu_release("24.04", "desktop")
}

pub fn r_kali() -> Result<Resolved> {
    plain(latest_in_listing(
        "https://cdimage.kali.org/current/",
        r#"href="(kali-linux-[\d.]+-installer-netinst-amd64\.iso)""#,
    ))
}

pub fn r_qubes() -> Result<Resolved> {
    plain(latest_in_listing(
        "https://ftp.qubes-os.org/iso/",
        r#"href="(Qubes-R[\d.]+-x86_64\.iso)""#,
    ))
}

/// Newest ISO of one Proxmox product; they share a single index. Matched up to
/// the "_<ver>-<build>" suffix so "proxmox-mail-gateway" skips the legacy
/// "proxmox-mailgateway_7.3-1.iso" in the same directory.
fn proxmox_iso(product: &str) -> Result<Resolved> {
    plain(latest_in_listing(
        "https://enterprise.proxmox.com/iso/",
        &format!(
            r#"href="\.?/?({}_[\d.]+-\d+\.iso)""#,
            regex::escape(product)
        ),
    ))
}

pub fn r_proxmox() -> Result<Resolved> {
    proxmox_iso("proxmox-ve")
}
pub fn r_proxmox_backup() -> Result<Resolved> {
    proxmox_iso("proxmox-backup-server")
}
pub fn r_proxmox_mailgw() -> Result<Resolved> {
    proxmox_iso("proxmox-mail-gateway")
}
pub fn r_proxmox_dcm() -> Result<Resolved> {
    proxmox_iso("proxmox-datacenter-manager")
}

pub fn r_xcpng() -> Result<Resolved> {
    // The numeric dir filter skips the "drivers" dir. Full ISO, not netinstall.
    // A respin appends ".2" to the build date and sorts above the original.
    latest_subdir_iso(
        "https://updates.xcp-ng.org/isos/",
        r#"href="(xcp-ng-[\d.]+-\d+(?:\.\d+)?\.iso)""#,
        "",
        DIR_REGEX,
    )
}

pub fn r_nixos_graphical() -> Result<Resolved> {
    // Newest stable channel from the NixOS channel S3 bucket, then its ISO.
    let xml = http_get("https://nix-channels.s3.amazonaws.com/?delimiter=/")?;
    let ver = max_ver(find_all(r"<Prefix>nixos-(\d\d\.\d\d)/</Prefix>", &xml)?)
        .ok_or_else(|| anyhow!("could not list NixOS channels"))?;
    Ok(Resolved {
        url: format!(
            "https://channels.nixos.org/nixos-{ver}/latest-nixos-graphical-x86_64-linux.iso"
        ),
        name: format!("nixos-graphical-{ver}-x86_64.iso"),
        mirrors: Vec::new(),
    })
}

/// Pointer files are PGP-signed; extract the "<ts>/<file>.iso" path. Upstream
/// names the file after the build recipe, which says nothing in a Ventoy menu,
/// so store "<label>-amd64-<date>.iso"; the date keeps it version-bearing.
fn gentoo(pointer: &str, label: &str) -> Result<Resolved> {
    let base = "https://distfiles.gentoo.org/releases/amd64/autobuilds/";
    let txt = http_get(&format!("{base}{pointer}"))?;
    let c = Regex::new(r"((\d{8})T\d{6}Z/\S+?\.iso)")?
        .captures(&txt)
        .ok_or_else(|| anyhow!("no ISO path in Gentoo pointer {pointer}"))?;
    Ok((
        format!("{base}{}", &c[1]),
        format!("{label}-amd64-{}.iso", &c[2]),
    )
        .into())
}

pub fn r_gentoo_minimal() -> Result<Resolved> {
    gentoo("latest-install-amd64-minimal.txt", "gentoo-minimal")
}
pub fn r_gentoo_livegui() -> Result<Resolved> {
    gentoo("latest-livegui-amd64.txt", "gentoo-livegui")
}

pub fn r_omarchy() -> Result<Resolved> {
    // The releases atom feed avoids GitHub API rate limits; first entry = newest.
    let atom = http_get("https://github.com/basecamp/omarchy/releases.atom")?;
    let tag = Regex::new(r"/releases/tag/v?([\d.]+)")?
        .captures(&atom)
        .ok_or_else(|| anyhow!("could not find latest Omarchy release"))?[1]
        .to_string();
    // 4.5 renamed the image to "-x86"; the stick name stays so the manifest follows it.
    Ok((
        format!("https://iso.omarchy.org/omarchy-{tag}-x86.iso"),
        format!("omarchy-{tag}.iso"),
    )
        .into())
}

/// SourceForge's real direct-download URL (302s to a binary mirror). The legacy
/// downloads.sourceforge.net/project/... form now serves an HTML page.
pub fn sf_download(proj: &str, rel_path: &str) -> String {
    format!("https://sourceforge.net/projects/{proj}/files/{rel_path}/download")
}

fn sourceforge_rss(proj: &str, path: &str, iso_regex: &str) -> Result<Resolved> {
    let rss = http_get(&format!(
        "https://sourceforge.net/projects/{proj}/rss?path={path}"
    ))?;
    let want = Regex::new(&format!("{iso_regex}/download"))?;
    for link in find_all(
        r"<link>(https://sourceforge\.net/projects/[^<]+?/download)</link>",
        &rss,
    )? {
        if want.is_match(&link) {
            let (_, after) = link
                .split_once("/files/")
                .context("no /files/ in SourceForge link")?;
            let rel = after.rsplit_once("/download").map_or(after, |(r, _)| r);
            let name = rel.rsplit('/').next().unwrap_or(rel);
            return Ok((sf_download(proj, rel), name.to_string()).into());
        }
    }
    bail!("no ISO in SourceForge RSS for {proj}")
}

pub fn r_systemrescue() -> Result<Resolved> {
    sourceforge_rss(
        "systemrescuecd",
        "/sysresccd-x86",
        r"systemrescue-[\d.]+-amd64\.iso",
    )
}

pub fn r_gparted() -> Result<Resolved> {
    sourceforge_rss(
        "gparted",
        "/gparted-live-stable",
        r"gparted-live-[\d.\-]+-amd64\.iso",
    )
}

pub fn r_clonezilla() -> Result<Resolved> {
    sourceforge_rss(
        "clonezilla",
        "/clonezilla_live_stable",
        r"clonezilla-live-[\d.\-]+-amd64\.iso",
    )
}

pub fn r_dban() -> Result<Resolved> {
    // Final release, project unmaintained -> static.
    Ok((
        sf_download("dban", "dban/dban-2.3.0/dban-2.3.0_i586.iso"),
        "dban-2.3.0_i586.iso".into(),
    )
        .into())
}

pub fn r_hbcd() -> Result<Resolved> {
    // The filename never carries a version: the manifest diffs on size / Last-Modified.
    Ok(fixed(
        "https://www.hirensbootcd.org/files/HBCD_PE_x64.iso",
        "HBCD_PE_x64.iso",
    ))
}

fn fixed(url: &str, name: &str) -> Resolved {
    (url.to_string(), name.to_string()).into()
}

// ---- extra resolvers (best-effort; a detect failure just skips that item) ----

/// (url, name) of the newest release asset matching asset_regex.
fn github_asset(repo: &str, asset_regex: &str) -> Result<Resolved> {
    let data: Value = serde_json::from_str(&http_get(&format!(
        "https://api.github.com/repos/{repo}/releases/latest"
    ))?)?;
    let re = Regex::new(asset_regex)?;
    for a in data["assets"].as_array().into_iter().flatten() {
        let name = a["name"].as_str().context("asset without name")?;
        if re.is_match(name) {
            let url = a["browser_download_url"]
                .as_str()
                .context("asset without URL")?;
            return Ok(fixed(url, name));
        }
    }
    bail!("no asset matching {asset_regex:?} in {repo}")
}

const DIR_REGEX: &str = r#"href="([\d.]+)/""#;

/// Highest-versioned subdirectory of an autoindex, then the newest ISO inside
/// it (optionally under a further `sub` path).
fn latest_subdir_iso(base: &str, iso_regex: &str, sub: &str, dir_regex: &str) -> Result<Resolved> {
    let d = max_ver(find_all(dir_regex, &http_get(base)?)?)
        .ok_or_else(|| anyhow!("no version dir at {base}"))?;
    plain(latest_in_listing(&format!("{base}{d}/{sub}"), iso_regex))
}

// ---- desktop distros ----

pub fn r_mint() -> Result<Resolved> {
    latest_subdir_iso(
        "https://mirrors.edge.kernel.org/linuxmint/stable/",
        r#"href="(linuxmint-[\d.]+-cinnamon-64bit\.iso)""#,
        "",
        DIR_REGEX,
    )
}

/// Newest *released* ISO of an official Ubuntu flavour from cdimage.
///
/// A version directory shows up months before the release is done (only
/// snapshot-N/ subdirs, no ISO), and not every flavour ships every release
/// (Ubuntu MATE skipped 26.04), so walk newest-first and take the first with an
/// image. `lts` only considers even-year .04 releases: for a flavour that skips
/// an LTS, the interim fallback goes end-of-life after nine months while the
/// previous LTS is still supported.
fn ubuntu_flavour(flavour: &str, edition: &str, lts: bool) -> Result<Resolved> {
    let base = format!("https://cdimage.ubuntu.com/{flavour}/releases/");
    let lts_re = Regex::new(r"^\d*[02468]\.04(\.\d+)*$")?;
    let dirs = sorted_desc(
        find_all(DIR_REGEX, &http_get(&base)?)?
            .into_iter()
            .filter(|d| !lts || lts_re.is_match(d)),
    );
    let rgx = format!(
        r#"href="({}-[\d.]+-{edition}-amd64\.iso)""#,
        regex::escape(flavour)
    );
    for d in dirs.iter().take(4) {
        if let Ok(found) = latest_in_listing(&format!("{base}{d}/release/"), &rgx) {
            return Ok(found.into());
        }
    }
    bail!("no released {flavour} ISO found")
}

pub fn r_kubuntu() -> Result<Resolved> {
    ubuntu_flavour("kubuntu", "desktop", false)
}
pub fn r_xubuntu() -> Result<Resolved> {
    ubuntu_flavour("xubuntu", "desktop", false)
}
pub fn r_lubuntu() -> Result<Resolved> {
    ubuntu_flavour("lubuntu", "desktop", false)
}
pub fn r_ubuntu_mate() -> Result<Resolved> {
    ubuntu_flavour("ubuntu-mate", "desktop", true)
}
pub fn r_ubuntu_budgie() -> Result<Resolved> {
    ubuntu_flavour("ubuntu-budgie", "desktop", false)
}
pub fn r_ubuntu_cinn() -> Result<Resolved> {
    ubuntu_flavour("ubuntucinnamon", "desktop", false)
}
pub fn r_ubuntu_studio() -> Result<Resolved> {
    ubuntu_flavour("ubuntustudio", "desktop", false)
}
pub fn r_edubuntu() -> Result<Resolved> {
    ubuntu_flavour("edubuntu", "desktop", false)
}
pub fn r_ubuntu_unity() -> Result<Resolved> {
    ubuntu_flavour("ubuntu-unity", "desktop", false)
}
pub fn r_ubuntu_kylin() -> Result<Resolved> {
    ubuntu_flavour("ubuntukylin", "desktop", false)
}

pub fn r_fedora_ws() -> Result<Resolved> {
    fedora_iso(
        "Workstation",
        r#"href="(Fedora-Workstation-Live-[^"]+\.iso)""#,
    )
}

pub fn r_fedora_server() -> Result<Resolved> {
    fedora_iso("Server", r#"href="(Fedora-Server-dvd-x86_64-[^"]+\.iso)""#)
}

pub fn r_debian_live_gnome() -> Result<Resolved> {
    plain(latest_in_listing(
        "https://cdimage.debian.org/debian-cd/current-live/amd64/iso-hybrid/",
        r#"href="(debian-live-[\d.]+-amd64-gnome\.iso)""#,
    ))
}

const LEAP_BASE: &str = "https://download.opensuse.org/distribution/leap/";

/// Leap 16 replaced the DVD image with a smaller offline installer under a new
/// subdirectory and filename scheme; both layouts are served, newest first. The
/// 16.x name ends in ".install.iso", not the "-Build<n>" snapshots beside it.
const LEAP_LAYOUTS: &[(&str, &str)] = &[
    (
        "offline/",
        r#"href="\.?/?(Leap-[\d.]+-offline-installer-x86_64\.install\.iso)""#,
    ),
    (
        "iso/",
        r#"href="\.?/?(openSUSE-Leap-[\d.]+-DVD-x86_64-Media\.iso)""#,
    ),
];

/// Leap release directories, newest first. Leap renumbered 42.x -> 15.x, so
/// the legacy 42 line is older despite the higher number and is excluded.
pub fn leap_versions() -> Result<Vec<String>> {
    let dirs = find_all(r#"href="\./([\d.]+)/""#, &http_get(LEAP_BASE)?)?;
    Ok(sorted_desc(dirs.into_iter().filter(|d| {
        d.split('.')
            .next()
            .and_then(|m| m.parse::<u64>().ok())
            .is_some_and(|m| m < 40)
    })))
}

pub fn r_opensuse_leap() -> Result<Resolved> {
    for d in leap_versions()? {
        for (sub, rgx) in LEAP_LAYOUTS {
            if let Ok(found) = latest_in_listing(&format!("{LEAP_BASE}{d}/{sub}"), rgx) {
                return Ok(found.into());
            }
        }
    }
    bail!("no openSUSE Leap installer ISO found")
}

pub fn r_opensuse_tumbleweed() -> Result<Resolved> {
    Ok(fixed(
        "https://download.opensuse.org/tumbleweed/iso/openSUSE-Tumbleweed-DVD-x86_64-Current.iso",
        "openSUSE-Tumbleweed-DVD-x86_64-Current.iso",
    ))
}

pub fn r_endeavouros() -> Result<Resolved> {
    plain(latest_in_listing(
        "https://mirror.moson.org/endeavouros/iso/",
        r#"href="(EndeavourOS_[^"]+\.iso)""#,
    ))
}

pub fn r_cachyos() -> Result<Resolved> {
    latest_subdir_iso(
        "https://mirror.cachyos.org/ISO/desktop/",
        r#"href="(cachyos-desktop-linux-[^"]+\.iso)""#,
        "",
        r#"href="([\d]{6})/""#,
    )
}

pub fn r_void() -> Result<Resolved> {
    plain(latest_in_listing(
        "https://repo-default.voidlinux.org/live/current/",
        r#"href="(void-live-x86_64-\d+-xfce\.iso)""#,
    ))
}

pub fn r_alpine() -> Result<Resolved> {
    plain(latest_in_listing(
        "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/",
        r#"href="(alpine-standard-[\d.]+-x86_64\.iso)""#,
    ))
}

pub fn r_mxlinux() -> Result<Resolved> {
    sourceforge_rss("mx-linux", "/", r"MX-[\d.]+(?:_[A-Za-z]+)?_x64\.iso")
}

pub fn r_slackware() -> Result<Resolved> {
    latest_subdir_iso(
        "https://mirrors.slackware.com/slackware/slackware-iso/",
        r#"href="(slackware64-[\d.]+-install-dvd\.iso)""#,
        "",
        r#"href="(slackware64-[\d.]+-iso)/""#,
    )
}

// ---- server / enterprise ----

pub fn r_rocky() -> Result<Resolved> {
    // 9.x uses "-dvd.iso", 10.x uses "-dvd1.iso"; match both.
    latest_subdir_iso(
        "https://download.rockylinux.org/pub/rocky/",
        r#"href="(Rocky-[\d.]+-x86_64-dvd1?\.iso)""#,
        "isos/x86_64/",
        DIR_REGEX,
    )
}

pub fn r_alma() -> Result<Resolved> {
    // repo.almalinux.org sends an incomplete cert chain -> a mirror with a complete one.
    latest_subdir_iso(
        "https://mirror.rackspace.com/almalinux/",
        r#"href="(AlmaLinux-[\d.]+-x86_64-dvd\.iso)""#,
        "isos/x86_64/",
        DIR_REGEX,
    )
}

pub fn r_centos_stream() -> Result<Resolved> {
    let base = "https://mirror.stream.centos.org/";
    let n = find_all(r#"href="(\d+)-stream/""#, &http_get(base)?)?
        .iter()
        .filter_map(|x| x.parse::<u64>().ok())
        .max()
        .ok_or_else(|| anyhow!("no CentOS Stream release at {base}"))?;
    plain(latest_in_listing(
        &format!("{base}{n}-stream/BaseOS/x86_64/iso/"),
        r#"href="(CentOS-Stream-[^"]+-dvd1\.iso)""#,
    ))
}

// ---- security ----

pub fn r_parrot() -> Result<Resolved> {
    latest_subdir_iso(
        "https://deb.parrot.sh/parrot/iso/",
        r#"href="(Parrot-security-[\d.]+_amd64\.iso)""#,
        "",
        DIR_REGEX,
    )
}

// ---- rescue / utility tools ----

pub fn r_rescuezilla() -> Result<Resolved> {
    github_asset("rescuezilla/rescuezilla", r"64bit\.\w+\.iso$")
}

pub fn r_supergrub2() -> Result<Resolved> {
    sourceforge_rss(
        "supergrub2",
        "/",
        r"supergrub2-classic-[\d.a-z]+-multiarch-CD\.iso",
    )
}

pub fn r_rescatux() -> Result<Resolved> {
    sourceforge_rss("rescatux", "/", r"rescatux[-_][\d.]+\.iso")
}

pub fn r_bootrepair() -> Result<Resolved> {
    sourceforge_rss("boot-repair-cd", "/", r"boot-repair-disk-64bit\.iso")
}

pub fn r_shredos() -> Result<Resolved> {
    // Modern, maintained disk wiper (nwipe), a DBAN successor.
    github_asset("PartialVolume/shredos.x86_64", r"x86-64.*\.iso$")
}

pub fn r_finnix() -> Result<Resolved> {
    latest_subdir_iso(
        "https://www.finnix.org/releases/",
        r#"href="(finnix-[\d.]+\.iso)""#,
        "",
        DIR_REGEX,
    )
}

pub fn r_grml() -> Result<Resolved> {
    // "full" ships the complete toolset; "small" is the stripped variant.
    plain(latest_in_listing(
        "https://download.grml.org/",
        r#"href="(grml-full-[\d.]+-amd64\.iso)""#,
    ))
}

pub fn r_kaspersky() -> Result<Resolved> {
    Ok(fixed(
        "https://rescuedisk.s.kaspersky-labs.com/updatable/2018/krd.iso",
        "krd.iso",
    ))
}

pub fn r_eset() -> Result<Resolved> {
    Ok(fixed(
        "https://download.eset.com/com/eset/tools/recovery/current/eset_sysrescue_live_enu.iso",
        "eset_sysrescue_live_enu.iso",
    ))
}

pub fn r_ubcd() -> Result<Resolved> {
    Ok(fixed(
        "https://mirror.koddos.net/ubcd/ubcd539.iso",
        "ubcd539.iso",
    ))
}

// ---- BSD / other ----

pub const FREEBSD_BASE: &str =
    "https://ftp.halifax.rwth-aachen.de/freebsd/releases/amd64/amd64/ISO-IMAGES/";

/// r_freebsd with the fetcher passed in. The next release's directory appears
/// weeks early with only BETA/RC images; the previous one is current then.
/// Only a missing RELEASE image falls back, an HTTP error does not.
pub fn freebsd_with(get: Get) -> Result<(String, String)> {
    let rgx = r#"href="(FreeBSD-[\d.]+-RELEASE-amd64-disc1\.iso)""#;
    let dirs = sorted_desc(find_all(DIR_REGEX, &get(FREEBSD_BASE)?)?);
    if dirs.len() < 2 {
        bail!("no version dirs at {FREEBSD_BASE}");
    }
    let listing = |d: &str| -> Result<(String, String)> {
        let base = format!("{FREEBSD_BASE}{d}/");
        let html = get(&base)?;
        Ok((base, html))
    };
    let (base, html) = listing(&dirs[0])?;
    pick_latest(&base, &html, rgx).or_else(|_| {
        crate::style::note(format!(
            "FreeBSD {} has no RELEASE image yet, using {}",
            dirs[0], dirs[1]
        ));
        let (base, html) = listing(&dirs[1])?;
        pick_latest(&base, &html, rgx)
    })
}

pub fn r_freebsd() -> Result<Resolved> {
    // download.freebsd.org sends an incomplete cert chain -> a mirror with a complete one.
    plain(freebsd_with(&net::http_get_cached))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_is_a_subset_without_repeats() {
        let v: Vec<String> = (0..9).map(|i| i.to_string()).collect();
        let s = sample(v.clone(), 5);
        assert_eq!(s.len(), 5);
        assert_eq!(s.iter().collect::<BTreeSet<_>>().len(), 5);
        assert!(s.iter().all(|x| v.contains(x)));
        assert_eq!(sample(v[..2].to_vec(), 5).len(), 2);
    }

    #[test]
    fn max_ver_and_sort() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            max_ver(v(&["9.6", "10.0", "10.0", "drivers"])).as_deref(),
            Some("10.0")
        );
        assert_eq!(
            sorted_desc(v(&["15.6", "16.0", "15.10"])),
            v(&["16.0", "15.10", "15.6"])
        );
    }
}
