//! The catalog of downloadable items (Python: CATALOG, DESCRIPTIONS, TIERS).

use crate::app::Preset;
use crate::resolvers as r;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Standard,
    Advanced,
    Extra,
    Windows,
}

/// What a resolver found: download URL, target filename, mirror candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub url: String,
    pub name: String,
    pub mirrors: Vec<String>,
}

impl From<(String, String)> for Resolved {
    fn from((url, name): (String, String)) -> Self {
        Resolved {
            url,
            name,
            mirrors: Vec::new(),
        }
    }
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
    /// Approximate download size in MB (decimal), for fitting a stick.
    pub size_mb: u32,
}

const WIN11_PAGE: &str = "https://massgrave.dev/windows_11_links";
const WIN10_PAGE: &str = "https://massgrave.dev/windows_10_links";
const WINSERVER_PAGE: &str = "https://massgrave.dev/windows-server-links";

const fn it(
    key: &'static str,
    label: &'static str,
    category: &'static str,
    tier: Tier,
    kind: Kind,
    desc: &'static str,
    size_mb: u32,
) -> Item {
    Item {
        key,
        label,
        category,
        tier,
        kind,
        desc,
        size_mb,
    }
}

// Items keep their category order for the pickers. Desktop is ordered by family
// (Arch-based, Ubuntu/Debian-based, Fedora, SUSE, Gentoo, independents), Rescue
// by job (repair, partitioning/imaging, boot repair, wiping, virus scan,
// hardware). Windows is in no preset on purpose: it cannot be fetched
// unattended, so it is offered through a separate opt-in prompt. `server` keeps
// Windows Server on the massgrave hand-off under --windows fido/uup, neither of
// which covers Server.
static CATALOG: &[Item] = &[
    // ---- Desktop
    it(
        "arch",
        "Arch Linux",
        "Desktop",
        Tier::Standard,
        Kind::Iso(r::r_arch),
        "rolling-release, build-it-yourself; text installer",
        1300,
    ),
    it(
        "omarchy",
        "Omarchy",
        "Desktop",
        Tier::Standard,
        Kind::Iso(r::r_omarchy),
        "preconfigured Arch + Hyprland desktop, ready to use",
        6500,
    ),
    it(
        "endeavour",
        "EndeavourOS",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_endeavouros),
        "Arch made easy -- graphical installer, sane defaults",
        3900,
    ),
    it(
        "cachyos",
        "CachyOS",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_cachyos),
        "performance-tuned Arch spin, graphical installer",
        3200,
    ),
    it(
        "ubuntu_desk26",
        "Ubuntu 26.04 Desktop",
        "Desktop",
        Tier::Standard,
        Kind::Iso(r::r_ubuntu_desktop_2604),
        "newest Ubuntu, easiest start for beginners",
        6500,
    ),
    it(
        "ubuntu_desk24",
        "Ubuntu 24.04 Desktop",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_ubuntu_desktop_2404),
        "previous Ubuntu LTS, supported until 2029",
        6200,
    ),
    it(
        "mint",
        "Linux Mint Cinnamon",
        "Desktop",
        Tier::Standard,
        Kind::Iso(r::r_mint),
        "beginner-friendly Ubuntu spin, classic Windows-like desktop",
        3100,
    ),
    it(
        "mxlinux",
        "MX Linux",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_mxlinux),
        "lightweight Debian spin, runs well on old hardware",
        2900,
    ),
    it(
        "debian_live",
        "Debian Live GNOME",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_debian_live_gnome),
        "plain Debian as a live desktop -- try it before installing",
        3800,
    ),
    it(
        "fedora_kde",
        "Fedora KDE Live",
        "Desktop",
        Tier::Standard,
        Kind::Iso(r::r_fedora_kde),
        "modern KDE Plasma desktop, live-bootable",
        3500,
    ),
    it(
        "fedora_ws",
        "Fedora Workstation",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_fedora_ws),
        "same Fedora with GNOME instead of KDE",
        2900,
    ),
    it(
        "opensuse_lp",
        "openSUSE Leap",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_opensuse_leap),
        "stable openSUSE release, enterprise-grade base",
        4500,
    ),
    it(
        "opensuse_tw",
        "openSUSE Tumbleweed",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_opensuse_tumbleweed),
        "the rolling openSUSE, always-newest packages",
        4700,
    ),
    it(
        "gentoo_gui",
        "Gentoo LiveGUI",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_gentoo_livegui),
        "Gentoo as a ready-made live desktop -- try before compiling",
        4300,
    ),
    it(
        "gentoo_min",
        "Gentoo minimal",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_gentoo_minimal),
        "the same Gentoo as a text-only installer, built from source",
        1000,
    ),
    it(
        "nixos",
        "NixOS graphical",
        "Desktop",
        Tier::Standard,
        Kind::Iso(r::r_nixos_graphical),
        "declarative OS -- whole system defined in one config file",
        3900,
    ),
    it(
        "void",
        "Void Linux (xfce)",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_void),
        "independent distro, runit instead of systemd, very lean",
        1400,
    ),
    it(
        "alpine",
        "Alpine Linux",
        "Desktop",
        Tier::Advanced,
        Kind::Iso(r::r_alpine),
        "tiny musl/busybox distro, popular for containers",
        400,
    ),
    it(
        "slackware",
        "Slackware64",
        "Desktop",
        Tier::Extra,
        Kind::Iso(r::r_slackware),
        "oldest surviving distro, minimal and unopinionated",
        3800,
    ),
    // ---- Ubuntu flavours
    it(
        "kubuntu",
        "Kubuntu",
        "Ubuntu flavours",
        Tier::Advanced,
        Kind::Iso(r::r_kubuntu),
        "Ubuntu with KDE Plasma; configurable, Windows-like",
        5100,
    ),
    it(
        "xubuntu",
        "Xubuntu",
        "Ubuntu flavours",
        Tier::Advanced,
        Kind::Iso(r::r_xubuntu),
        "Ubuntu with Xfce; light and classic, good on older PCs",
        5300,
    ),
    it(
        "lubuntu",
        "Lubuntu",
        "Ubuntu flavours",
        Tier::Advanced,
        Kind::Iso(r::r_lubuntu),
        "lightest Ubuntu (LXQt); revives weak/old hardware",
        4000,
    ),
    it(
        "ubuntu_mate",
        "Ubuntu MATE",
        "Ubuntu flavours",
        Tier::Extra,
        Kind::Iso(r::r_ubuntu_mate),
        "Ubuntu with MATE, the traditional GNOME 2 desktop (LTS)",
        5200,
    ),
    it(
        "ubuntu_budgie",
        "Ubuntu Budgie",
        "Ubuntu flavours",
        Tier::Extra,
        Kind::Iso(r::r_ubuntu_budgie),
        "Ubuntu with Budgie; tidy modern desktop, little fuss",
        4200,
    ),
    it(
        "ubuntu_cinn",
        "Ubuntu Cinnamon",
        "Ubuntu flavours",
        Tier::Extra,
        Kind::Iso(r::r_ubuntu_cinn),
        "Ubuntu with Mint's Cinnamon desktop",
        4900,
    ),
    it(
        "ubuntu_studio",
        "Ubuntu Studio",
        "Ubuntu flavours",
        Tier::Extra,
        Kind::Iso(r::r_ubuntu_studio),
        "Ubuntu for audio/video/graphics work, low-latency kernel",
        6900,
    ),
    it(
        "edubuntu",
        "Edubuntu",
        "Ubuntu flavours",
        Tier::Extra,
        Kind::Iso(r::r_edubuntu),
        "Ubuntu preloaded with educational software for schools",
        7200,
    ),
    it(
        "ubuntu_unity",
        "Ubuntu Unity",
        "Ubuntu flavours",
        Tier::Extra,
        Kind::Iso(r::r_ubuntu_unity),
        "Ubuntu with the classic Unity 7 desktop",
        4700,
    ),
    it(
        "ubuntu_kylin",
        "Ubuntu Kylin",
        "Ubuntu flavours",
        Tier::Extra,
        Kind::Iso(r::r_ubuntu_kylin),
        "official Ubuntu for Chinese users, UKUI desktop",
        5600,
    ),
    // ---- Server
    it(
        "debian",
        "Debian netinst",
        "Server",
        Tier::Standard,
        Kind::Iso(r::r_debian),
        "small net installer; rock-stable server base",
        800,
    ),
    it(
        "ubuntu_srv26",
        "Ubuntu 26.04 Server",
        "Server",
        Tier::Standard,
        Kind::Iso(r::r_ubuntu_server_2604),
        "newest Ubuntu LTS server, no desktop",
        2900,
    ),
    it(
        "ubuntu_srv24",
        "Ubuntu 24.04 Server",
        "Server",
        Tier::Advanced,
        Kind::Iso(r::r_ubuntu_server_2404),
        "previous Ubuntu LTS server, supported until 2029",
        4100,
    ),
    it(
        "proxmox",
        "Proxmox VE",
        "Server",
        Tier::Standard,
        Kind::Iso(r::r_proxmox),
        "bare-metal hypervisor for VMs and LXC containers",
        1700,
    ),
    it(
        "pbs",
        "Proxmox Backup Server",
        "Server",
        Tier::Advanced,
        Kind::Iso(r::r_proxmox_backup),
        "backup server for Proxmox VE -- deduplicated, incremental",
        1500,
    ),
    it(
        "pmg",
        "Proxmox Mail Gateway",
        "Server",
        Tier::Extra,
        Kind::Iso(r::r_proxmox_mailgw),
        "mail gateway: spam and virus filter in front of a mail server",
        1700,
    ),
    it(
        "pdm",
        "Proxmox Datacenter Mgr",
        "Server",
        Tier::Extra,
        Kind::Iso(r::r_proxmox_dcm),
        "single pane of glass for several Proxmox VE hosts",
        1500,
    ),
    it(
        "xcpng",
        "XCP-ng",
        "Server",
        Tier::Advanced,
        Kind::Iso(r::r_xcpng),
        "the other open hypervisor, XenServer-based; Xen Orchestra UI",
        700,
    ),
    it(
        "rocky",
        "Rocky Linux",
        "Server",
        Tier::Advanced,
        Kind::Iso(r::r_rocky),
        "free RHEL clone for servers",
        10200,
    ),
    it(
        "alma",
        "AlmaLinux",
        "Server",
        Tier::Advanced,
        Kind::Iso(r::r_alma),
        "free RHEL clone, alternative to Rocky",
        10100,
    ),
    it(
        "centos",
        "CentOS Stream",
        "Server",
        Tier::Extra,
        Kind::Iso(r::r_centos_stream),
        "upstream preview of the next RHEL",
        10800,
    ),
    it(
        "fedora_srv",
        "Fedora Server",
        "Server",
        Tier::Extra,
        Kind::Iso(r::r_fedora_server),
        "Fedora as a server install, no desktop",
        4100,
    ),
    // ---- Security
    it(
        "kali",
        "Kali netinst",
        "Security",
        Tier::Standard,
        Kind::Iso(r::r_kali),
        "pentesting distro; net installer, pulls tools on install",
        700,
    ),
    it(
        "parrot",
        "Parrot Security",
        "Security",
        Tier::Advanced,
        Kind::Iso(r::r_parrot),
        "pentesting + privacy tools, lighter than Kali",
        8700,
    ),
    it(
        "qubes",
        "Qubes OS",
        "Security",
        Tier::Advanced,
        Kind::Iso(r::r_qubes),
        "security by isolation -- every app in its own VM",
        8500,
    ),
    // ---- Rescue
    it(
        "systemrescue",
        "SystemRescue",
        "Rescue",
        Tier::Standard,
        Kind::Iso(r::r_systemrescue),
        "Arch-based repair system; partitioning, data recovery",
        1400,
    ),
    it(
        "hbcd",
        "Hiren's BootCD PE",
        "Rescue",
        Tier::Standard,
        Kind::Iso(r::r_hbcd),
        "Windows PE toolkit; repair and diagnose Windows offline",
        3300,
    ),
    it(
        "finnix",
        "Finnix (sysadmin)",
        "Rescue",
        Tier::Extra,
        Kind::Iso(r::r_finnix),
        "small text-only live system for sysadmin work",
        600,
    ),
    it(
        "grml",
        "Grml (sysadmin)",
        "Rescue",
        Tier::Advanced,
        Kind::Iso(r::r_grml),
        "Debian-based sysadmin live system, heavy on shell tooling",
        1200,
    ),
    it(
        "gparted",
        "GParted Live",
        "Rescue",
        Tier::Standard,
        Kind::Iso(r::r_gparted),
        "graphical partition editor; resize/move/format disks",
        700,
    ),
    it(
        "clonezilla",
        "Clonezilla Live",
        "Rescue",
        Tier::Standard,
        Kind::Iso(r::r_clonezilla),
        "disk and partition imaging, clone drives 1:1",
        500,
    ),
    it(
        "rescuezilla",
        "Rescuezilla",
        "Rescue",
        Tier::Advanced,
        Kind::Iso(r::r_rescuezilla),
        "the same imaging with a friendly GUI, reads Clonezilla images",
        1600,
    ),
    it(
        "bootrepair",
        "Boot-Repair-Disk",
        "Rescue",
        Tier::Advanced,
        Kind::Iso(r::r_bootrepair),
        "one-click repair for GRUB/EFI boot problems",
        2600,
    ),
    it(
        "supergrub2",
        "Super Grub2 Disk",
        "Rescue",
        Tier::Advanced,
        Kind::Iso(r::r_supergrub2),
        "boots a system whose bootloader is broken, without fixing it",
        100,
    ),
    it(
        "rescatux",
        "Rescatux",
        "Rescue",
        Tier::Extra,
        Kind::Iso(r::r_rescatux),
        "boot repair plus Windows/Linux password reset",
        700,
    ),
    it(
        "shredos",
        "ShredOS (disk wiper)",
        "Rescue",
        Tier::Standard,
        Kind::Iso(r::r_shredos),
        "secure disk eraser, handles SSD/NVMe properly",
        400,
    ),
    it(
        "dban",
        "DBAN (final)",
        "Rescue",
        Tier::Extra,
        Kind::Iso(r::r_dban),
        "its ancient predecessor, HDD only -- kept for old hardware",
        100,
    ),
    it(
        "kaspersky",
        "Kaspersky Rescue Disk",
        "Rescue",
        Tier::Extra,
        Kind::Iso(r::r_kaspersky),
        "offline virus scan for an infected Windows install",
        700,
    ),
    it(
        "eset",
        "ESET SysRescue Live",
        "Rescue",
        Tier::Extra,
        Kind::Iso(r::r_eset),
        "offline virus scan, second opinion to Kaspersky",
        1000,
    ),
    it(
        "memtest",
        "MemTest86 (PassMark)",
        "Rescue",
        Tier::Standard,
        Kind::Memtest,
        "RAM test, finds faulty memory modules",
        1100,
    ),
    it(
        "ubcd",
        "Ultimate Boot CD",
        "Rescue",
        Tier::Extra,
        Kind::Iso(r::r_ubcd),
        "collection of DOS-era hardware diagnostic tools",
        800,
    ),
    // ---- BSD/Other
    it(
        "freebsd",
        "FreeBSD",
        "BSD/Other",
        Tier::Extra,
        Kind::Iso(r::r_freebsd),
        "not Linux -- the BSD server/NAS operating system",
        1400,
    ),
    // ---- Windows
    it(
        "win11_de",
        "Windows 11 German",
        "Windows",
        Tier::Windows,
        Kind::Windows(WinSpec {
            win: "11",
            rel: "25H2",
            lang: "German",
            page: WIN11_PAGE,
            pick: "Win 11 Consumer 25H2 (Latest) -> German (de-de)",
            tmpl: "Win_11_25H2_German_{}.iso",
            server: false,
        }),
        "official Win 11 25H2 installer, German",
        7700,
    ),
    it(
        "win11_en",
        "Windows 11 English",
        "Windows",
        Tier::Windows,
        Kind::Windows(WinSpec {
            win: "11",
            rel: "25H2",
            lang: "English",
            page: WIN11_PAGE,
            pick: "Win 11 Consumer 25H2 (Latest) -> English (en-us)",
            tmpl: "Win_11_25H2_English_{}.iso",
            server: false,
        }),
        "official Win 11 25H2 installer, English",
        7700,
    ),
    it(
        "win10_en",
        "Windows 10 English",
        "Windows",
        Tier::Windows,
        Kind::Windows(WinSpec {
            win: "10",
            rel: "22H2",
            lang: "English",
            page: WIN10_PAGE,
            pick: "Win 10 22H2 (final) -> English",
            tmpl: "Win_10_22H2_English_{}.iso",
            server: false,
        }),
        "official Win 10 22H2 installer, English",
        6100,
    ),
    it(
        "win10_de",
        "Windows 10 German",
        "Windows",
        Tier::Windows,
        Kind::Windows(WinSpec {
            win: "10",
            rel: "22H2",
            lang: "German",
            page: WIN10_PAGE,
            pick: "Win 10 22H2 (final) -> German",
            tmpl: "Win_10_22H2_German_{}.iso",
            server: false,
        }),
        "official Win 10 22H2 installer, German",
        6100,
    ),
    // ---- Windows Server
    it(
        "winsrv2025_en",
        "Windows Server 2025 English",
        "Windows Server",
        Tier::Windows,
        Kind::Windows(WinSpec {
            win: "2025",
            rel: "2025",
            lang: "English",
            page: WINSERVER_PAGE,
            pick: "Windows Server 2025 -> English (en-us)",
            tmpl: "Win_Server_2025_English_{}.iso",
            server: true,
        }),
        "Windows Server 2025 -- current LTSC, English",
        7400,
    ),
    it(
        "winsrv2025_de",
        "Windows Server 2025 German",
        "Windows Server",
        Tier::Windows,
        Kind::Windows(WinSpec {
            win: "2025",
            rel: "2025",
            lang: "German",
            page: WINSERVER_PAGE,
            pick: "Windows Server 2025 -> German (de-de)",
            tmpl: "Win_Server_2025_German_{}.iso",
            server: true,
        }),
        "Windows Server 2025 -- current LTSC, German",
        7400,
    ),
    it(
        "winsrv2022_en",
        "Windows Server 2022 English",
        "Windows Server",
        Tier::Windows,
        Kind::Windows(WinSpec {
            win: "2022",
            rel: "2022",
            lang: "English",
            page: WINSERVER_PAGE,
            pick: "Windows Server 2022 -> English (en-us)",
            tmpl: "Win_Server_2022_English_{}.iso",
            server: true,
        }),
        "Windows Server 2022 -- previous LTSC, English",
        5300,
    ),
    it(
        "winsrv2022_de",
        "Windows Server 2022 German",
        "Windows Server",
        Tier::Windows,
        Kind::Windows(WinSpec {
            win: "2022",
            rel: "2022",
            lang: "German",
            page: WINSERVER_PAGE,
            pick: "Windows Server 2022 -> German (de-de)",
            tmpl: "Win_Server_2022_German_{}.iso",
            server: true,
        }),
        "Windows Server 2022 -- previous LTSC, German",
        5300,
    ),
];

/// All items, in picker order.
pub fn catalog() -> &'static [Item] {
    CATALOG
}

pub fn find(key: &str) -> Option<&'static Item> {
    catalog().iter().find(|it| it.key == key)
}

/// Items of a preset; Windows is in none (Custom returns nothing).
pub fn preset_items(preset: Preset) -> Vec<&'static Item> {
    let tiers: &[Tier] = match preset {
        Preset::Standard => &[Tier::Standard],
        Preset::Advanced => &[Tier::Standard, Tier::Advanced],
        Preset::Everything => &[Tier::Standard, Tier::Advanced, Tier::Extra],
        Preset::Fit | Preset::Custom => &[],
    };
    CATALOG
        .iter()
        .filter(|it| tiers.contains(&it.tier))
        .collect()
}

/// Approximate total size of `items` in bytes.
pub fn total_bytes(items: &[&Item]) -> u64 {
    items
        .iter()
        .map(|it| u64::from(it.size_mb) * 1_000_000)
        .sum()
}

/// Smallest common stick size that holds `bytes` with ~5 % to spare after
/// formatting; None
/// above 1 TB.
pub fn stick_size_for(bytes: u64) -> Option<&'static str> {
    const SIZES: [(u64, &str); 8] = [
        (8, "8 GB"),
        (16, "16 GB"),
        (32, "32 GB"),
        (64, "64 GB"),
        (128, "128 GB"),
        (256, "256 GB"),
        (512, "512 GB"),
        (1000, "1 TB"),
    ];
    SIZES
        .iter()
        // A "256 GB" stick formats to about 97 % of that, and 5 % stays free.
        .find(|(gb, _)| bytes <= gb * 1_000_000_000 / 100 * 92)
        .map(|(_, name)| *name)
}

/// Tried first within their category when space is short: the beginner
/// desktops, the plain servers, the everyday rescue tools. The rest follows
/// in catalog order.
const FIT_FIRST: &[&str] = &[
    "mint",
    "ubuntu_desk26",
    "fedora_kde",
    "debian",
    "ubuntu_srv26",
    "systemrescue",
    "hbcd",
    "gparted",
    "clonezilla",
];

/// The set that fits `budget` bytes (keeping 5 % headroom). First what is
/// already on the stick (`have`: it stays there either way) and MemTest86, then
/// tier by tier, one item per category per round in catalog order, so a small
/// stick gets a desktop, a server and a rescue tool before seconds of anything.
/// Windows never. Returned in catalog order.
pub fn fit(budget: u64, have: &[&str]) -> Vec<&'static Item> {
    let mut left = budget / 100 * 95;
    let mut add = |it: &Item, force: bool| {
        let size = u64::from(it.size_mb) * 1_000_000;
        let fits = size <= left;
        if fits || force {
            left = left.saturating_sub(size);
        }
        fits || force
    };
    // What is on the stick stays either way, so it is always part of the set.
    let mut picked: Vec<&'static Item> = CATALOG
        .iter()
        .filter(|it| it.tier != Tier::Windows && have.contains(&it.key) && add(it, true))
        .collect();
    picked.extend(CATALOG.iter().filter(|it| {
        matches!(it.kind, Kind::Memtest) && !have.contains(&it.key) && add(it, false)
    }));
    for tier in [Tier::Standard, Tier::Advanced, Tier::Extra] {
        let mut queues: Vec<(&str, std::collections::VecDeque<&'static Item>)> = Vec::new();
        for it in CATALOG.iter().filter(|it| it.tier == tier) {
            if matches!(it.kind, Kind::Memtest) || have.contains(&it.key) {
                continue;
            }
            match queues.iter_mut().find(|(c, _)| *c == it.category) {
                Some((_, q)) => q.push_back(it),
                None => queues.push((it.category, [it].into())),
            }
        }
        for (_, q) in &mut queues {
            q.make_contiguous().sort_by_key(|it| {
                FIT_FIRST
                    .iter()
                    .position(|k| *k == it.key)
                    .unwrap_or(usize::MAX)
            });
        }
        while queues.iter().any(|(_, q)| !q.is_empty()) {
            for (_, q) in &mut queues {
                // What does not fit now never will: the budget only shrinks.
                while let Some(it) = q.pop_front() {
                    if add(it, false) {
                        picked.push(it);
                        break;
                    }
                }
            }
        }
    }
    let pos = |it: &Item| CATALOG.iter().position(|c| c.key == it.key);
    picked.sort_by_key(|it| pos(it));
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_invariants() {
        let keys: Vec<_> = CATALOG.iter().map(|it| it.key).collect();
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "duplicate catalog keys");
        assert_eq!(keys.len(), 69);
        assert!(
            CATALOG.iter().all(|it| !it.desc.is_empty()),
            "every key needs a desc"
        );
        for p in [
            Preset::Standard,
            Preset::Advanced,
            Preset::Everything,
            Preset::Custom,
        ] {
            assert!(
                preset_items(p).iter().all(|it| it.tier != Tier::Windows),
                "Windows in {p:?}"
            );
        }
        assert!(preset_items(Preset::Custom).is_empty());
        assert_eq!(preset_items(Preset::Everything).len(), 61);
        let Kind::Windows(w) = find("winsrv2022_de").unwrap().kind else {
            panic!()
        };
        assert!(w.server && w.tmpl == "Win_Server_2022_German_{}.iso");
    }

    #[test]
    fn stick_sizes_round_up_with_headroom() {
        let gb = |n: f64| (n * 1e9) as u64;
        assert_eq!(stick_size_for(0), Some("8 GB"));
        assert_eq!(stick_size_for(gb(7.3)), Some("8 GB"));
        assert_eq!(stick_size_for(gb(7.4)), Some("16 GB")); // 92 % of 8 GB is 7.36
        assert_eq!(stick_size_for(gb(38.3)), Some("64 GB"));
        assert_eq!(stick_size_for(gb(207.0)), Some("256 GB"));
        assert_eq!(stick_size_for(gb(257.0)), Some("512 GB"));
        assert_eq!(stick_size_for(gb(920.0)), Some("1 TB"));
        assert_eq!(stick_size_for(gb(921.0)), None);
    }

    #[test]
    fn fit_sticks() {
        let standard = preset_items(Preset::Standard);
        for gb in [8u64, 16, 32, 64, 128] {
            // What an exFAT-formatted stick of that size has free.
            let budget = gb * 1_000_000_000 / 100 * 97;
            let set = fit(budget, &[]);
            let keys: Vec<_> = set.iter().map(|it| it.key).collect();
            println!(
                "{gb:>3} GB: {:>2} items, {:>5.1} GB  {keys:?}",
                set.len(),
                total_bytes(&set) as f64 / 1e9
            );
            assert!(total_bytes(&set) <= budget / 100 * 95);
            assert!(set.iter().all(|it| it.tier != Tier::Windows));
            for cat in ["Desktop", "Server", "Rescue"] {
                assert!(
                    set.iter().any(|it| it.category == cat),
                    "{gb} GB lacks {cat}"
                );
            }
            assert!(keys.contains(&"memtest"));
            if gb >= 64 {
                assert!(
                    standard.iter().all(|s| keys.contains(&s.key)),
                    "{gb} GB misses Standard"
                );
            }
        }
        assert!(fit(0, &[]).is_empty());
        assert_eq!(
            fit(u64::MAX / 2, &[]).len(),
            preset_items(Preset::Everything).len()
        );
        // What is on the stick is always in, even an Extra item that alone
        // exceeds the budget; Windows never.
        let keys: Vec<_> = fit(3_000_000_000, &["slackware", "win11_de"])
            .iter()
            .map(|it| it.key)
            .collect();
        assert_eq!(keys, ["slackware"]);
        // Small sticks get the beginner desktop first.
        assert!(
            fit(8_000_000_000 / 100 * 97, &[])
                .iter()
                .any(|it| it.key == "mint")
        );
    }
}
