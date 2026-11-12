//! The catalog of downloadable items (Python: CATALOG, DESCRIPTIONS, TIERS).

use crate::app::Preset;
use crate::resolvers as r;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier { Standard, Advanced, Extra, Windows }

/// What a resolver found: download URL, target filename, mirror candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub url: String,
    pub name: String,
    pub mirrors: Vec<String>,
}

impl From<(String, String)> for Resolved {
    fn from((url, name): (String, String)) -> Self { Resolved { url, name, mirrors: Vec::new() } }
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

const WIN11_PAGE: &str = "https://massgrave.dev/windows_11_links";
const WIN10_PAGE: &str = "https://massgrave.dev/windows_10_links";
const WINSERVER_PAGE: &str = "https://massgrave.dev/windows-server-links";

const fn it(key: &'static str, label: &'static str, category: &'static str, tier: Tier, kind: Kind,
            desc: &'static str) -> Item {
    Item { key, label, category, tier, kind, desc }
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
    it("arch", "Arch Linux", "Desktop", Tier::Standard, Kind::Iso(r::r_arch),
        "rolling-release, build-it-yourself; text installer"),
    it("omarchy", "Omarchy", "Desktop", Tier::Standard, Kind::Iso(r::r_omarchy),
        "preconfigured Arch + Hyprland desktop, ready to use"),
    it("endeavour", "EndeavourOS", "Desktop", Tier::Advanced, Kind::Iso(r::r_endeavouros),
        "Arch made easy -- graphical installer, sane defaults"),
    it("cachyos", "CachyOS", "Desktop", Tier::Advanced, Kind::Iso(r::r_cachyos),
        "performance-tuned Arch spin, graphical installer"),
    it("ubuntu_desk26", "Ubuntu 26.04 Desktop", "Desktop", Tier::Standard, Kind::Iso(r::r_ubuntu_desktop_2604),
        "newest Ubuntu, easiest start for beginners"),
    it("ubuntu_desk24", "Ubuntu 24.04 Desktop", "Desktop", Tier::Advanced, Kind::Iso(r::r_ubuntu_desktop_2404),
        "previous Ubuntu LTS, supported until 2029"),
    it("mint", "Linux Mint Cinnamon", "Desktop", Tier::Standard, Kind::Iso(r::r_mint),
        "beginner-friendly Ubuntu spin, classic Windows-like desktop"),
    it("mxlinux", "MX Linux", "Desktop", Tier::Advanced, Kind::Iso(r::r_mxlinux),
        "lightweight Debian spin, runs well on old hardware"),
    it("debian_live", "Debian Live GNOME", "Desktop", Tier::Advanced, Kind::Iso(r::r_debian_live_gnome),
        "plain Debian as a live desktop -- try it before installing"),
    it("fedora_kde", "Fedora KDE Live", "Desktop", Tier::Standard, Kind::Iso(r::r_fedora_kde),
        "modern KDE Plasma desktop, live-bootable"),
    it("fedora_ws", "Fedora Workstation", "Desktop", Tier::Advanced, Kind::Iso(r::r_fedora_ws),
        "same Fedora with GNOME instead of KDE"),
    it("opensuse_lp", "openSUSE Leap", "Desktop", Tier::Advanced, Kind::Iso(r::r_opensuse_leap),
        "stable openSUSE release, enterprise-grade base"),
    it("opensuse_tw", "openSUSE Tumbleweed", "Desktop", Tier::Advanced, Kind::Iso(r::r_opensuse_tumbleweed),
        "the rolling openSUSE, always-newest packages"),
    it("gentoo_gui", "Gentoo LiveGUI", "Desktop", Tier::Advanced, Kind::Iso(r::r_gentoo_livegui),
        "Gentoo as a ready-made live desktop -- try before compiling"),
    it("gentoo_min", "Gentoo minimal", "Desktop", Tier::Advanced, Kind::Iso(r::r_gentoo_minimal),
        "the same Gentoo as a text-only installer, built from source"),
    it("nixos", "NixOS graphical", "Desktop", Tier::Standard, Kind::Iso(r::r_nixos_graphical),
        "declarative OS -- whole system defined in one config file"),
    it("void", "Void Linux (xfce)", "Desktop", Tier::Advanced, Kind::Iso(r::r_void),
        "independent distro, runit instead of systemd, very lean"),
    it("alpine", "Alpine Linux", "Desktop", Tier::Advanced, Kind::Iso(r::r_alpine),
        "tiny musl/busybox distro, popular for containers"),
    it("slackware", "Slackware64", "Desktop", Tier::Extra, Kind::Iso(r::r_slackware),
        "oldest surviving distro, minimal and unopinionated"),
    // ---- Ubuntu flavours
    it("kubuntu", "Kubuntu", "Ubuntu flavours", Tier::Advanced, Kind::Iso(r::r_kubuntu),
        "Ubuntu with KDE Plasma; configurable, Windows-like"),
    it("xubuntu", "Xubuntu", "Ubuntu flavours", Tier::Advanced, Kind::Iso(r::r_xubuntu),
        "Ubuntu with Xfce; light and classic, good on older PCs"),
    it("lubuntu", "Lubuntu", "Ubuntu flavours", Tier::Advanced, Kind::Iso(r::r_lubuntu),
        "lightest Ubuntu (LXQt); revives weak/old hardware"),
    it("ubuntu_mate", "Ubuntu MATE", "Ubuntu flavours", Tier::Extra, Kind::Iso(r::r_ubuntu_mate),
        "Ubuntu with MATE, the traditional GNOME 2 desktop (LTS)"),
    it("ubuntu_budgie", "Ubuntu Budgie", "Ubuntu flavours", Tier::Extra, Kind::Iso(r::r_ubuntu_budgie),
        "Ubuntu with Budgie; tidy modern desktop, little fuss"),
    it("ubuntu_cinn", "Ubuntu Cinnamon", "Ubuntu flavours", Tier::Extra, Kind::Iso(r::r_ubuntu_cinn),
        "Ubuntu with Mint's Cinnamon desktop"),
    it("ubuntu_studio", "Ubuntu Studio", "Ubuntu flavours", Tier::Extra, Kind::Iso(r::r_ubuntu_studio),
        "Ubuntu for audio/video/graphics work, low-latency kernel"),
    it("edubuntu", "Edubuntu", "Ubuntu flavours", Tier::Extra, Kind::Iso(r::r_edubuntu),
        "Ubuntu preloaded with educational software for schools"),
    it("ubuntu_unity", "Ubuntu Unity", "Ubuntu flavours", Tier::Extra, Kind::Iso(r::r_ubuntu_unity),
        "Ubuntu with the classic Unity 7 desktop"),
    it("ubuntu_kylin", "Ubuntu Kylin", "Ubuntu flavours", Tier::Extra, Kind::Iso(r::r_ubuntu_kylin),
        "official Ubuntu for Chinese users, UKUI desktop"),
    // ---- Server
    it("debian", "Debian netinst", "Server", Tier::Standard, Kind::Iso(r::r_debian),
        "small net installer; rock-stable server base"),
    it("ubuntu_srv26", "Ubuntu 26.04 Server", "Server", Tier::Standard, Kind::Iso(r::r_ubuntu_server_2604),
        "newest Ubuntu LTS server, no desktop"),
    it("ubuntu_srv24", "Ubuntu 24.04 Server", "Server", Tier::Advanced, Kind::Iso(r::r_ubuntu_server_2404),
        "previous Ubuntu LTS server, supported until 2029"),
    it("proxmox", "Proxmox VE", "Server", Tier::Standard, Kind::Iso(r::r_proxmox),
        "bare-metal hypervisor for VMs and LXC containers"),
    it("pbs", "Proxmox Backup Server", "Server", Tier::Advanced, Kind::Iso(r::r_proxmox_backup),
        "backup server for Proxmox VE -- deduplicated, incremental"),
    it("pmg", "Proxmox Mail Gateway", "Server", Tier::Extra, Kind::Iso(r::r_proxmox_mailgw),
        "mail gateway: spam and virus filter in front of a mail server"),
    it("pdm", "Proxmox Datacenter Mgr", "Server", Tier::Extra, Kind::Iso(r::r_proxmox_dcm),
        "single pane of glass for several Proxmox VE hosts"),
    it("xcpng", "XCP-ng", "Server", Tier::Advanced, Kind::Iso(r::r_xcpng),
        "the other open hypervisor, XenServer-based; Xen Orchestra UI"),
    it("rocky", "Rocky Linux", "Server", Tier::Advanced, Kind::Iso(r::r_rocky),
        "free RHEL clone for servers"),
    it("alma", "AlmaLinux", "Server", Tier::Advanced, Kind::Iso(r::r_alma),
        "free RHEL clone, alternative to Rocky"),
    it("centos", "CentOS Stream", "Server", Tier::Extra, Kind::Iso(r::r_centos_stream),
        "upstream preview of the next RHEL"),
    it("fedora_srv", "Fedora Server", "Server", Tier::Extra, Kind::Iso(r::r_fedora_server),
        "Fedora as a server install, no desktop"),
    // ---- Security
    it("kali", "Kali netinst", "Security", Tier::Standard, Kind::Iso(r::r_kali),
        "pentesting distro; net installer, pulls tools on install"),
    it("parrot", "Parrot Security", "Security", Tier::Advanced, Kind::Iso(r::r_parrot),
        "pentesting + privacy tools, lighter than Kali"),
    it("qubes", "Qubes OS", "Security", Tier::Advanced, Kind::Iso(r::r_qubes),
        "security by isolation -- every app in its own VM"),
    // ---- Rescue
    it("systemrescue", "SystemRescue", "Rescue", Tier::Standard, Kind::Iso(r::r_systemrescue),
        "Arch-based repair system; partitioning, data recovery"),
    it("hbcd", "Hiren's BootCD PE", "Rescue", Tier::Standard, Kind::Iso(r::r_hbcd),
        "Windows PE toolkit; repair and diagnose Windows offline"),
    it("finnix", "Finnix (sysadmin)", "Rescue", Tier::Extra, Kind::Iso(r::r_finnix),
        "small text-only live system for sysadmin work"),
    it("grml", "Grml (sysadmin)", "Rescue", Tier::Advanced, Kind::Iso(r::r_grml),
        "Debian-based sysadmin live system, heavy on shell tooling"),
    it("gparted", "GParted Live", "Rescue", Tier::Standard, Kind::Iso(r::r_gparted),
        "graphical partition editor; resize/move/format disks"),
    it("clonezilla", "Clonezilla Live", "Rescue", Tier::Standard, Kind::Iso(r::r_clonezilla),
        "disk and partition imaging, clone drives 1:1"),
    it("rescuezilla", "Rescuezilla", "Rescue", Tier::Advanced, Kind::Iso(r::r_rescuezilla),
        "the same imaging with a friendly GUI, reads Clonezilla images"),
    it("bootrepair", "Boot-Repair-Disk", "Rescue", Tier::Advanced, Kind::Iso(r::r_bootrepair),
        "one-click repair for GRUB/EFI boot problems"),
    it("supergrub2", "Super Grub2 Disk", "Rescue", Tier::Advanced, Kind::Iso(r::r_supergrub2),
        "boots a system whose bootloader is broken, without fixing it"),
    it("rescatux", "Rescatux", "Rescue", Tier::Extra, Kind::Iso(r::r_rescatux),
        "boot repair plus Windows/Linux password reset"),
    it("shredos", "ShredOS (disk wiper)", "Rescue", Tier::Standard, Kind::Iso(r::r_shredos),
        "secure disk eraser, handles SSD/NVMe properly"),
    it("dban", "DBAN (final)", "Rescue", Tier::Extra, Kind::Iso(r::r_dban),
        "its ancient predecessor, HDD only -- kept for old hardware"),
    it("kaspersky", "Kaspersky Rescue Disk", "Rescue", Tier::Extra, Kind::Iso(r::r_kaspersky),
        "offline virus scan for an infected Windows install"),
    it("eset", "ESET SysRescue Live", "Rescue", Tier::Extra, Kind::Iso(r::r_eset),
        "offline virus scan, second opinion to Kaspersky"),
    it("memtest", "MemTest86 (PassMark)", "Rescue", Tier::Standard, Kind::Memtest,
        "RAM test, finds faulty memory modules"),
    it("ubcd", "Ultimate Boot CD", "Rescue", Tier::Extra, Kind::Iso(r::r_ubcd),
        "collection of DOS-era hardware diagnostic tools"),
    // ---- BSD/Other
    it("freebsd", "FreeBSD", "BSD/Other", Tier::Extra, Kind::Iso(r::r_freebsd),
        "not Linux -- the BSD server/NAS operating system"),
    // ---- Windows
    it("win11_de", "Windows 11 German", "Windows", Tier::Windows, Kind::Windows(WinSpec { win: "11", rel: "25H2", lang: "German", page: WIN11_PAGE,
        pick: "Win 11 Consumer 25H2 (Latest) -> German (de-de)",
        tmpl: "Win_11_25H2_German_{}.iso", server: false }),
        "official Win 11 25H2 installer, German"),
    it("win11_en", "Windows 11 English", "Windows", Tier::Windows, Kind::Windows(WinSpec { win: "11", rel: "25H2", lang: "English", page: WIN11_PAGE,
        pick: "Win 11 Consumer 25H2 (Latest) -> English (en-us)",
        tmpl: "Win_11_25H2_English_{}.iso", server: false }),
        "official Win 11 25H2 installer, English"),
    it("win10_en", "Windows 10 English", "Windows", Tier::Windows, Kind::Windows(WinSpec { win: "10", rel: "22H2", lang: "English", page: WIN10_PAGE,
        pick: "Win 10 22H2 (final) -> English",
        tmpl: "Win_10_22H2_English_{}.iso", server: false }),
        "official Win 10 22H2 installer, English"),
    it("win10_de", "Windows 10 German", "Windows", Tier::Windows, Kind::Windows(WinSpec { win: "10", rel: "22H2", lang: "German", page: WIN10_PAGE,
        pick: "Win 10 22H2 (final) -> German",
        tmpl: "Win_10_22H2_German_{}.iso", server: false }),
        "official Win 10 22H2 installer, German"),
    // ---- Windows Server
    it("winsrv2025_en", "Windows Server 2025 English", "Windows Server", Tier::Windows, Kind::Windows(WinSpec { win: "2025", rel: "2025", lang: "English", page: WINSERVER_PAGE,
        pick: "Windows Server 2025 -> English (en-us)",
        tmpl: "Win_Server_2025_English_{}.iso", server: true }),
        "Windows Server 2025 -- current LTSC, English"),
    it("winsrv2025_de", "Windows Server 2025 German", "Windows Server", Tier::Windows, Kind::Windows(WinSpec { win: "2025", rel: "2025", lang: "German", page: WINSERVER_PAGE,
        pick: "Windows Server 2025 -> German (de-de)",
        tmpl: "Win_Server_2025_German_{}.iso", server: true }),
        "Windows Server 2025 -- current LTSC, German"),
    it("winsrv2022_en", "Windows Server 2022 English", "Windows Server", Tier::Windows, Kind::Windows(WinSpec { win: "2022", rel: "2022", lang: "English", page: WINSERVER_PAGE,
        pick: "Windows Server 2022 -> English (en-us)",
        tmpl: "Win_Server_2022_English_{}.iso", server: true }),
        "Windows Server 2022 -- previous LTSC, English"),
    it("winsrv2022_de", "Windows Server 2022 German", "Windows Server", Tier::Windows, Kind::Windows(WinSpec { win: "2022", rel: "2022", lang: "German", page: WINSERVER_PAGE,
        pick: "Windows Server 2022 -> German (de-de)",
        tmpl: "Win_Server_2022_German_{}.iso", server: true }),
        "Windows Server 2022 -- previous LTSC, German"),
];

/// All items, in picker order.
pub fn catalog() -> &'static [Item] { CATALOG }

pub fn find(key: &str) -> Option<&'static Item> {
    catalog().iter().find(|it| it.key == key)
}

/// Items of a preset; Windows is in none (Custom returns nothing).
pub fn preset_items(preset: Preset) -> Vec<&'static Item> {
    let tiers: &[Tier] = match preset {
        Preset::Standard => &[Tier::Standard],
        Preset::Advanced => &[Tier::Standard, Tier::Advanced],
        Preset::Everything => &[Tier::Standard, Tier::Advanced, Tier::Extra],
        Preset::Custom => &[],
    };
    CATALOG.iter().filter(|it| tiers.contains(&it.tier)).collect()
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
        assert!(CATALOG.iter().all(|it| !it.desc.is_empty()), "every key needs a desc");
        for p in [Preset::Standard, Preset::Advanced, Preset::Everything, Preset::Custom] {
            assert!(preset_items(p).iter().all(|it| it.tier != Tier::Windows), "Windows in {p:?}");
        }
        assert!(preset_items(Preset::Custom).is_empty());
        assert_eq!(preset_items(Preset::Everything).len(), 61);
        let Kind::Windows(w) = find("winsrv2022_de").unwrap().kind else { panic!() };
        assert!(w.server && w.tmpl == "Win_Server_2022_German_{}.iso");
    }
}
