#!/usr/bin/env python3
"""
update_ventoy_isos.py

On launch, this script AUTO-DETECTS the newest version of every ISO on the
Ventoy stick (directory listings, SourceForge RSS, GitHub API, the NixOS
channel bucket and the massgrave.dev markdown), then downloads them one after
another (never in parallel). Tails is excluded.

Cross-platform (Windows / Linux / macOS / WSL), Python 3.6+, standard library
only. Downloads run through urllib with resume support (HTTP Range) and show,
per file, the serving host plus the live speed in MB/s and Mbit/s. Where the
source publishes a checksum file next to the image, the finished download is
verified against it before it is put in place (--no-verify turns that off).

Each downloaded ISO's version is recorded in a manifest (ventoy_versions.json)
next to the ISOs on the stick. On every rerun the script compares the newest
version online against that record and only downloads what actually changed --
rolling releases that keep the same filename are checked via the remote file's
size / Last-Modified header. Use --force to re-download everything, or
--dry-run to see what a run would change without writing anything.

After the downloads the script sweeps the stick for ISOs that a newer release
has replaced -- including ones the manifest never knew about -- lists each one
with the file that superseded it and deletes them once you confirm
(--cleanup {ask,yes,no}).

Finally the auto_install section of ventoy/ventoy.json is rebuilt: every .xml in
/template/win11 is attached to each Windows 11 image on the stick, /template/
win10 likewise, so unattended-setup answer files stay attached across the build
renames that Windows ISO updates bring. Entries pointing elsewhere are treated
as hand-written and left untouched.

On start you choose what to download from a catalog of ~60 Linux distros,
hypervisors, Windows ISOs and rescue tools: a preset (Standard / Advanced /
Everything) or a custom pick (numbers/ranges like 1,3,5-9). Presets:
    Standard    -- one mainstream pick per job: the common desktops, a server
                   netinst, and the rescue tools you actually reach for
    Advanced    -- standard + the sibling releases (Fedora GNOME next to KDE,
                   Gentoo, the main Ubuntu flavours) and more rescue tools
    Everything  -- the whole catalog, niche and legacy items included
Skip the menu with --preset {standard,advanced,everything,custom}.

Before you pick, the script lists what the stick already holds (item, exact
filename, size) and marks those entries with a '*' in the picker, so nothing
gets chosen twice by accident. If there is anything on it, the menu also
offers "Update what's there": start from the current set and just refresh it.
That entry is the one to take on a stick this script has filled before -- it
keeps the selection you already made. On a fresh stick it isn't offered at
all, so there is nothing to think about the first time round.

If --dest is omitted, the script lists the detected removable USB drives and
lets you pick one interactively -- no need to type the drive letter. Detection
uses the Win32 drive type on Windows, the removable flag in /sys on Linux, and
/Volumes on macOS. Under WSL it asks Windows which drive is removable, or, if
that setup has .exe interop disabled, falls back to the drvfs mounts under
/mnt/ with the system drive left out.

For distros that publish a machine-readable mirror list (currently Arch), the
script briefly speed-tests the best-scored mirrors and downloads from the
fastest. The other sources are already CDN / geo-routed, so they are used as-is.
Disable the test with --no-mirror-test.

Windows ISOs are NOT part of any preset -- they cannot be fetched unattended,
so the script asks separately whether to add them. Say yes and it opens the
massgrave.dev pages, tells you exactly which entry to pick, then watches the
stick and your Downloads folder: whatever ISO you download is matched, renamed
to the canonical name and moved onto the stick automatically (--windows
massgrave, the default).

Two automatic alternatives exist but each has a catch: --windows fido resolves
an official direct Microsoft link (light, no admin) but is subject to
Microsoft's anti-bot "Sentinel"; --windows uup pulls the build from Windows
Update and assembles the ISO locally (no bot protection) but needs
administrator rights and roughly 40 minutes plus ~15 GB of scratch space.

Usage:
    python update_ventoy_isos.py                 # interactive drive picker
    python update_ventoy_isos.py --dest E:\\     # explicit target
    python update_ventoy_isos.py --dry-run       # report only, write nothing
    python update_ventoy_isos.py --force         # ignore manifest, redownload
"""

import argparse
import atexit
import hashlib
import io
import json
import os
import re
import shutil
import ssl
import subprocess
import sys
import tempfile
import textwrap
import time
import urllib.error
import urllib.request
import webbrowser
import zipfile
from urllib.parse import quote, urlencode, urljoin, urlparse

__version__ = "0.7.0"

UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) VentoyISOUpdater/%s" % __version__
CTX = ssl.create_default_context()

# Manifest of installed versions, written next to the ISOs on the stick.
MANIFEST_NAME = "ventoy_versions.json"

# Windows ISO methods (--windows), none of them fully unattended:
#   massgrave -- the default. Opens the massgrave.dev page, says which entry to
#                pick, then watches for the download and files it onto the
#                stick. Manual click, but nothing can block it.
#   uup       -- UUP dump: pulls the build straight from Windows Update servers
#                (no bot protection) and assembles the ISO locally with DISM.
#                Needs admin, ~40 minutes and ~15 GB of scratch space.
#   fido      -- Fido resolves an official, direct Microsoft download link.
#                Light and no admin, but subject to Microsoft's anti-bot
#                "Sentinel", so it fails unpredictably.
FIDO_URL = "https://raw.githubusercontent.com/pbatard/Fido/master/Fido.ps1"
UUP_API = "https://api.uupdump.net"
UUP_GET = "https://uupdump.net/get.php"


# --------------------------------------------------------------------------- #
# HTTP helpers
# --------------------------------------------------------------------------- #
def _auth_for(url):
    """Authorization header for the GitHub API when a token is in the env.

    Unauthenticated the API allows 60 requests an hour per IP. Three catalog
    entries and the Ventoy check use it, which is fine from a home connection
    but not from shared CI runners, where that budget is routinely already
    spent by someone else. A token raises the limit to 5000; without one
    nothing changes.
    """
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token and urlparse(url).netloc.lower() == "api.github.com":
        return {"Authorization": "Bearer %s" % token}
    return {}


def http_get(url, timeout=45):
    headers = {"User-Agent": UA}
    headers.update(_auth_for(url))
    req = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(req, timeout=timeout, context=CTX) as r:
        return r.read().decode("utf-8", "replace")


def _ua_for(url):
    """Pick a User-Agent per host. SourceForge serves a browser-like UA an
    HTML 'your download will start' interstitial and only redirects wget/curl
    to the actual binary, so downloads there must not look like a browser."""
    host = urlparse(url).netloc.lower()
    if "sourceforge" in host or "sf.net" in host:
        return "Wget/1.21"
    return UA


def remote_meta(url, timeout=30):
    """Best-effort (size, last_modified) of a remote file, following redirects.

    Tries HEAD first; if the server rejects HEAD, falls back to a 1-byte ranged
    GET (which still exposes the full size via Content-Range). Returns
    (None, None) if nothing could be determined.
    """
    ua = _ua_for(url)

    def _read(req):
        with urllib.request.urlopen(req, timeout=timeout, context=CTX) as r:
            if "html" in r.headers.get("Content-Type", "").lower():
                raise RuntimeError("html")     # interstitial page, not the file
            cr = r.headers.get("Content-Range", "")
            m = re.search(r"/(\d+)\s*$", cr)
            size = int(m.group(1)) if m else (
                int(r.headers["Content-Length"])
                if r.headers.get("Content-Length") else None)
            return size, r.headers.get("Last-Modified")

    # Ranged GET first: it forces redirect chains (SourceForge, mirrors) to
    # resolve to the real binary and exposes the full size via Content-Range.
    try:
        req = urllib.request.Request(
            url, headers={"User-Agent": ua, "Range": "bytes=0-0"})
        return _read(req)
    except Exception:
        pass
    try:
        req = urllib.request.Request(url, method="HEAD",
                                     headers={"User-Agent": ua})
        return _read(req)
    except Exception:
        return None, None


# Suffixes for a sums file that belongs to one image, appended to its URL.
PER_FILE_SUMS = (".sha256", ".sha256sum", ".DIGESTS", ".sha256.txt")
# Names of a sums file covering a whole directory, resolved next to the image.
DIR_SUMS = ("SHA256SUMS", "sha256sum.txt", "sha256sums.txt", "CHECKSUM",
            "SHA256SUM")

_HASH = r"[0-9a-fA-F]{64}"


def _sha256_from_text(text, filename):
    """Pull the SHA-256 for `filename` out of a sums file's contents.

    Understands the coreutils form ("<hash>  <file>", optionally "*<file>" for
    binary mode) and the BSD form ("SHA256 (<file>) = <hash>").
    """
    quoted = re.escape(filename)
    for pattern in (r"^(%s)[ \t]+[*]?%s[ \t]*$" % (_HASH, quoted),
                    r"SHA256\s*\(\s*%s\s*\)\s*=\s*(%s)" % (quoted, _HASH)):
        found = re.search(pattern, text, re.M)
        if found:
            return found.group(1).lower()
    return None


def remote_sha256(url, filename, timeout=20):
    """Best-effort SHA-256 for an image, from whatever the source publishes.

    Returns None when nothing is published -- SourceForge redirects and vendor
    one-off URLs have no sums at all, and an unverified download still beats no
    download.

    A per-image sums file is addressed by the image URL itself, so a single
    hash in it can be trusted even when the name inside differs -- NixOS writes
    the upstream build name there while this script stores a shortened one. The
    word boundary keeps a 128-character SHA-512 from being read as a SHA-256,
    and requiring exactly one match keeps that shortcut off multi-entry files.
    SourceForge answers these URLs with an HTML interstitial, which matches
    nothing here and is therefore ignored on its own.
    """
    for suffix in PER_FILE_SUMS:
        try:
            text = http_get(url + suffix, timeout=timeout)
        except Exception:
            continue
        direct = _sha256_from_text(text, filename)
        if direct:
            return direct
        lone = re.findall(r"\b%s\b" % _HASH, text)
        if len(lone) == 1:
            return lone[0].lower()

    base = url.rsplit("/", 1)[0] + "/"
    for candidate in DIR_SUMS:
        try:
            text = http_get(base + candidate, timeout=timeout)
        except Exception:
            continue
        found = _sha256_from_text(text, filename)
        if found:
            return found
    return None


def sha256_file(path, chunk=1024 * 1024):
    """SHA-256 of a file, read in chunks so a 5 GB ISO stays out of memory."""
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(chunk), b""):
            digest.update(block)
    return digest.hexdigest()


def _fmt_size(n):
    """Human-readable byte count (binary units)."""
    x = float(n)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if x < 1024:
            return "%.1f %s" % (x, unit)
        x /= 1024
    return "%.1f TiB" % x


SPEEDTEST_SECONDS = 2.0        # measured window per mirror
SPEEDTEST_WARMUP = 512 * 1024  # bytes discarded before the clock starts


def measure_speed(url, duration=SPEEDTEST_SECONDS, timeout=8):
    """Measure a mirror's throughput in bytes/sec over a fixed time window.

    Timing starts only after SPEEDTEST_WARMUP bytes have arrived, so TCP
    slow-start and the TLS handshake don't drag the result down -- a short
    byte-counted sample makes fast mirrors look slow and fluctuates between
    runs.

    The window is also capped in bytes (~80 MB) so picking a mirror never
    costs more traffic than it saves. Below ~320 Mbit/s a mirror gets the
    full `duration`; above that the cap ends the sample early, which is
    harmless -- by then the measurement is already stable to ~1%."""
    cap = int(duration * 40 * 1024 * 1024) + SPEEDTEST_WARMUP   # ~320 Mbit/s
    req = urllib.request.Request(
        url, headers={"User-Agent": _ua_for(url), "Range": "bytes=0-%d" % (cap - 1)})
    try:
        started = time.monotonic()
        warm = got = 0
        t0 = None
        with urllib.request.urlopen(req, timeout=timeout, context=CTX) as r:
            while True:
                chunk = r.read(64 * 1024)
                if not chunk:
                    break
                if t0 is None:
                    warm += len(chunk)
                    if warm >= SPEEDTEST_WARMUP:
                        t0 = time.monotonic()      # start the measured window
                    elif time.monotonic() - started > timeout:
                        break
                    continue
                got += len(chunk)
                if time.monotonic() - t0 >= duration:
                    break
        if t0 is None:
            # Mirror never delivered even the warmup -- fall back to whatever
            # it managed, so a slow-but-alive host still beats a dead one.
            dt = time.monotonic() - started
            return warm / dt if warm and dt > 0 else 0.0
        dt = time.monotonic() - t0
        return got / dt if got and dt > 0 else 0.0
    except Exception:
        return 0.0


def fastest_mirror(urls):
    """Speed-test each candidate URL sequentially, return the fastest.

    Sequential (not parallel) so the mirrors don't compete for the same
    bandwidth and skew the measurement. Falls back to the first URL if none
    respond. All candidates are assumed to serve the identical file."""
    urls = list(dict.fromkeys(u for u in urls if u))   # dedupe, keep order
    if len(urls) <= 1:
        return urls[0]
    print("    speed-testing %d mirrors (up to %.0fs each)..."
          % (len(urls), SPEEDTEST_SECONDS))
    best_url, best_bps = urls[0], -1.0
    for u in urls:
        bps = measure_speed(u)
        host = urlparse(u).netloc
        if bps > 0:
            print("      %-42s %6.1f Mbit/s" % (host, bps * 8 / 1e6))
        else:
            print("      %-42s unreachable" % host)
        if bps > best_bps:
            best_url, best_bps = u, bps
    print("    -> fastest: %s" % urlparse(best_url).netloc)
    return best_url


def verkey(name):
    """Sort key from the integers in a filename, so 13.01 > 12.03, etc."""
    return tuple(int(n) for n in re.findall(r"\d+", name))


_LISTING_CACHE = {}


def http_get_cached(url, timeout=45):
    """http_get, but each index is fetched only once per run.

    Several catalog entries resolve off one and the same page: the four Proxmox
    products share a directory, the three Fedora editions share the release
    list. Without this they hit that host once per entry, and
    enterprise.proxmox.com answers four simultaneous requests by timing out the
    TLS handshake. Runs are short, so a value can never go stale within one.
    """
    if url not in _LISTING_CACHE:
        _LISTING_CACHE[url] = http_get(url, timeout=timeout)
    return _LISTING_CACHE[url]


def latest_in_listing(base, file_regex):
    """Return (url, filename) of the highest-versioned file in an autoindex."""
    html = http_get_cached(base)
    files = re.findall(file_regex, html)
    if not files:
        raise RuntimeError("no file matching %r at %s" % (file_regex, base))
    best = max(set(files), key=verkey)
    return urljoin(base, best), best


# --------------------------------------------------------------------------- #
# Version resolvers  ->  each returns (url, filename)
# --------------------------------------------------------------------------- #
def _arch_mirrors(limit=6):
    """Best-scored HTTPS Arch mirrors (lower score = healthier/faster) as full
    ISO URLs, for the fastest-mirror speed test."""
    try:
        data = json.loads(http_get("https://archlinux.org/mirrors/status/json/"))
    except Exception:
        return []
    good = [m for m in data.get("urls", [])
            if m.get("protocol") == "https" and m.get("active")
            and (m.get("completion_pct") or 0) >= 1.0 and m.get("score")]
    good.sort(key=lambda m: m["score"])
    return [m["url"].rstrip("/") + "/iso/latest/archlinux-x86_64.iso"
            for m in good[:limit]]


def r_arch():
    # /latest/ always points to the newest snapshot. Primary is the geo CDN;
    # extra candidates come from the mirror-status list for the speed test.
    primary = "https://geo.mirror.pkgbuild.com/iso/latest/archlinux-x86_64.iso"
    return primary, "archlinux-x86_64.iso", _arch_mirrors()


def r_debian():
    return latest_in_listing(
        "https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/",
        r'href="(debian-[\d.]+-amd64-netinst\.iso)"')


def _fedora_iso(edition, iso_regex):
    """Newest Fedora ISO of one edition (KDE, Workstation, Server, ...).

    dl.fedoraproject.org is behind an anti-bot challenge, so the release list
    comes from an open mirror.
    """
    rel_base = "https://mirrors.kernel.org/fedora/releases/"
    nums = [int(n) for n in re.findall(r'href="(\d+)/"',
                                       http_get_cached(rel_base))]
    if not nums:
        raise RuntimeError("could not list Fedora releases")
    iso_base = "%s%d/%s/x86_64/iso/" % (rel_base, max(nums), edition)
    return latest_in_listing(iso_base, iso_regex)


def r_fedora_kde():
    return _fedora_iso(
        "KDE", r'href="(Fedora-KDE-Desktop-Live-\d+-[\d.]+\.x86_64\.iso)"')


def r_ubuntu_server_2404():
    return latest_in_listing(
        "https://releases.ubuntu.com/24.04/",
        r'href="(ubuntu-24\.04(?:\.\d+)?-live-server-amd64\.iso)"')


def r_ubuntu_server_2604():
    return latest_in_listing(
        "https://releases.ubuntu.com/26.04/",
        r'href="(ubuntu-26\.04(?:\.\d+)?-live-server-amd64\.iso)"')


def r_ubuntu_desktop_2604():
    return latest_in_listing(
        "https://releases.ubuntu.com/26.04/",
        r'href="(ubuntu-26\.04(?:\.\d+)?-desktop-amd64\.iso)"')


def r_kali():
    return latest_in_listing(
        "https://cdimage.kali.org/current/",
        r'href="(kali-linux-[\d.]+-installer-netinst-amd64\.iso)"')


def r_qubes():
    return latest_in_listing(
        "https://ftp.qubes-os.org/iso/",
        r'href="(Qubes-R[\d.]+-x86_64\.iso)"')


def _proxmox_iso(product):
    """Newest ISO of one Proxmox product -- they share a single index.

    The product name is matched exactly up to the "_<ver>-<build>" suffix, so
    "proxmox-mail-gateway" does not also pick up the differently spelled legacy
    "proxmox-mailgateway_7.3-1.iso" that still sits in the same directory.
    """
    return latest_in_listing(
        "https://enterprise.proxmox.com/iso/",
        r'href="\.?/?(%s_[\d.]+-\d+\.iso)"' % re.escape(product))


def r_proxmox():
    return _proxmox_iso("proxmox-ve")


def r_proxmox_backup():
    return _proxmox_iso("proxmox-backup-server")


def r_proxmox_mailgw():
    return _proxmox_iso("proxmox-mail-gateway")


def r_proxmox_dcm():
    return _proxmox_iso("proxmox-datacenter-manager")


def r_xcpng():
    # Version dirs sit next to a non-numeric "drivers" dir, which the numeric
    # dir filter skips. Exclude the netinstall image: the full ISO is the one
    # that makes sense on a stick. A respin appends ".2" to the build date and
    # sorts above the original, which is what we want.
    return _latest_subdir_iso(
        "https://updates.xcp-ng.org/isos/",
        r'href="(xcp-ng-[\d.]+-\d+(?:\.\d+)?\.iso)"')


def r_nixos_graphical():
    # Newest stable channel from the NixOS channel S3 bucket, then its ISO.
    xml = http_get("https://nix-channels.s3.amazonaws.com/?delimiter=/")
    chans = re.findall(r"<Prefix>nixos-(\d\d\.\d\d)/</Prefix>", xml)
    if not chans:
        raise RuntimeError("could not list NixOS channels")
    ver = max(chans, key=verkey)
    return ("https://channels.nixos.org/nixos-%s/"
            "latest-nixos-graphical-x86_64-linux.iso" % ver,
            "nixos-graphical-%s-x86_64.iso" % ver)


def _gentoo(pointer, label):
    # Pointer files are PGP-signed; extract the "<ts>/<file>.iso" path.
    # Upstream names the file after the build recipe ("install-amd64-minimal-
    # <ts>.iso", "livegui-amd64-<ts>.iso"), which says neither "Gentoo" nor,
    # in the LiveGUI case, anything recognizable in a Ventoy boot menu -- so
    # store it as "<label>-amd64-<date>.iso" instead. The date keeps the
    # filename version-bearing, which is what the manifest diffs on.
    base = "https://distfiles.gentoo.org/releases/amd64/autobuilds/"
    txt = http_get(base + pointer)
    m = re.search(r"((\d{8})T\d{6}Z/\S+?\.iso)", txt)
    if not m:
        raise RuntimeError("no ISO path in Gentoo pointer %s" % pointer)
    rel, date = m.group(1), m.group(2)
    return base + rel, "%s-amd64-%s.iso" % (label, date)


def r_gentoo_minimal():
    return _gentoo("latest-install-amd64-minimal.txt", "gentoo-minimal")


def r_gentoo_livegui():
    return _gentoo("latest-livegui-amd64.txt", "gentoo-livegui")


def r_omarchy():
    # The releases atom feed avoids GitHub API rate limits; first entry = newest.
    atom = http_get("https://github.com/basecamp/omarchy/releases.atom")
    m = re.search(r"/releases/tag/v?([\d.]+)", atom)
    if not m:
        raise RuntimeError("could not find latest Omarchy release")
    tag = m.group(1)
    return "https://iso.omarchy.org/omarchy-%s.iso" % tag, "omarchy-%s.iso" % tag


def sf_download(proj, rel_path):
    """Build SourceForge's real direct-download URL (the .../files/.../download
    endpoint that 302-redirects to a binary mirror). The legacy
    downloads.sourceforge.net/project/... form now serves an HTML page."""
    return "https://sourceforge.net/projects/%s/files/%s/download" % (
        proj, rel_path)


def _sourceforge_rss(proj, path, iso_regex):
    rss = http_get("https://sourceforge.net/projects/%s/rss?path=%s" % (proj, path))
    links = re.findall(
        r"<link>(https://sourceforge\.net/projects/[^<]+?/download)</link>", rss)
    for link in links:
        if re.search(iso_regex + r"/download", link):
            rel = link.split("/files/", 1)[1].rsplit("/download", 1)[0]
            return sf_download(proj, rel), rel.split("/")[-1]
    raise RuntimeError("no ISO in SourceForge RSS for %s" % proj)


def r_systemrescue():
    return _sourceforge_rss("systemrescuecd", "/sysresccd-x86",
                            r"systemrescue-[\d.]+-amd64\.iso")


def r_gparted():
    return _sourceforge_rss("gparted", "/gparted-live-stable",
                            r"gparted-live-[\d.\-]+-amd64\.iso")


def r_clonezilla():
    return _sourceforge_rss("clonezilla", "/clonezilla_live_stable",
                            r"clonezilla-live-[\d.\-]+-amd64\.iso")


def r_dban():
    # Final release, project unmaintained -> static.
    return (sf_download("dban", "dban/dban-2.3.0/dban-2.3.0_i586.iso"),
            "dban-2.3.0_i586.iso")


def r_hbcd():
    # Fixed vendor URL: the filename never carries a version, so there is
    # nothing to detect and the manifest can only diff on size / Last-Modified.
    return ("https://www.hirensbootcd.org/files/HBCD_PE_x64.iso",
            "HBCD_PE_x64.iso")


# --------------------------------------------------------------------------- #
# Extra resolvers (best-effort; a detect failure just skips that item)
# --------------------------------------------------------------------------- #
def _github_asset(repo, asset_regex):
    """(url, name) of the newest release asset matching asset_regex."""
    data = json.loads(http_get(
        "https://api.github.com/repos/%s/releases/latest" % repo))
    for a in data.get("assets", []):
        if re.search(asset_regex, a["name"]):
            return a["browser_download_url"], a["name"]
    raise RuntimeError("no asset matching %r in %s" % (asset_regex, repo))


def _latest_subdir_iso(base, iso_regex, sub="", dir_regex=r'href="([\d.]+)/"'):
    """Find the highest-versioned subdirectory of an autoindex, then the
    newest ISO inside it (optionally under a further `sub` path)."""
    dirs = re.findall(dir_regex, http_get(base))
    if not dirs:
        raise RuntimeError("no version dir at %s" % base)
    d = max(dirs, key=verkey)
    return latest_in_listing("%s%s/%s" % (base, d, sub), iso_regex)


# ---- desktop distros ----------------------------------------------------- #
def r_mint():
    return _latest_subdir_iso(
        "https://mirrors.edge.kernel.org/linuxmint/stable/",
        r'href="(linuxmint-[\d.]+-cinnamon-64bit\.iso)"')


def r_ubuntu_desktop_2404():
    return latest_in_listing(
        "https://releases.ubuntu.com/24.04/",
        r'href="(ubuntu-24\.04(?:\.\d+)?-desktop-amd64\.iso)"')


def _ubuntu_flavour(flavour, edition="desktop"):
    """Newest *released* ISO of an official Ubuntu flavour from cdimage.

    A version directory shows up months before that release is finished --
    while 26.10 is in development its release/ holds only snapshot-N/ subdirs
    and no ISO -- and not every flavour ships every release (Ubuntu MATE
    skipped 26.04). So walk the versions newest-first and take the first one
    that actually has an image."""
    base = "https://cdimage.ubuntu.com/%s/releases/" % flavour
    dirs = sorted(set(re.findall(r'href="([\d.]+)/"', http_get(base))),
                  key=verkey, reverse=True)
    rgx = r'href="(%s-[\d.]+-%s-amd64\.iso)"' % (re.escape(flavour), edition)
    for d in dirs[:4]:
        try:
            return latest_in_listing("%s%s/release/" % (base, d), rgx)
        except Exception:
            continue
    raise RuntimeError("no released %s ISO found" % flavour)


def _flavour(name, edition="desktop"):
    """Catalog payload for one Ubuntu flavour -- ten near-identical named
    resolvers would be pure noise."""
    return lambda: _ubuntu_flavour(name, edition)


def r_fedora_ws():
    return _fedora_iso(
        "Workstation", r'href="(Fedora-Workstation-Live-[^"]+\.iso)"')


def r_fedora_server():
    return _fedora_iso(
        "Server", r'href="(Fedora-Server-dvd-x86_64-[^"]+\.iso)"')


def r_debian_live_gnome():
    return latest_in_listing(
        "https://cdimage.debian.org/debian-cd/current-live/amd64/iso-hybrid/",
        r'href="(debian-live-[\d.]+-amd64-gnome\.iso)"')


LEAP_BASE = "https://download.opensuse.org/distribution/leap/"

# Leap 16 replaced the single DVD image with a much smaller offline installer
# and moved it to a different subdirectory, under a different filename scheme.
# Both layouts are still served, newest first. Note the trailing ".install.iso"
# on the 16.x name -- the plain form, not the "-Build<n>" snapshots that sit in
# the same directory.
LEAP_LAYOUTS = (
    ("offline/",
     r'href="\.?/?(Leap-[\d.]+-offline-installer-x86_64\.install\.iso)"'),
    ("iso/",
     r'href="\.?/?(openSUSE-Leap-[\d.]+-DVD-x86_64-Media\.iso)"'),
)


def leap_versions():
    """Leap release directories, newest first, legacy 42.x line excluded.

    Leap renumbered 42.x -> 15.x, so 42 is the *older* line despite the higher
    number and must not win the sort.
    """
    dirs = re.findall(r'href="\./([\d.]+)/"', http_get(LEAP_BASE))
    return sorted((d for d in set(dirs) if int(d.split(".")[0]) < 40),
                  key=verkey, reverse=True)


def r_opensuse_leap():
    for d in leap_versions():
        for sub, rgx in LEAP_LAYOUTS:
            try:
                return latest_in_listing("%s%s/%s" % (LEAP_BASE, d, sub), rgx)
            except Exception:
                continue
    raise RuntimeError("no openSUSE Leap installer ISO found")


def r_opensuse_tumbleweed():
    return ("https://download.opensuse.org/tumbleweed/iso/"
            "openSUSE-Tumbleweed-DVD-x86_64-Current.iso",
            "openSUSE-Tumbleweed-DVD-x86_64-Current.iso")


def r_endeavouros():
    return latest_in_listing(
        "https://mirror.moson.org/endeavouros/iso/",
        r'href="(EndeavourOS_[^"]+\.iso)"')

def r_cachyos():
    return _latest_subdir_iso(
        "https://mirror.cachyos.org/ISO/desktop/",
        r'href="(cachyos-desktop-linux-[^"]+\.iso)"',
        dir_regex=r'href="([\d]{6}/)"')

def r_void():
    return latest_in_listing(
        "https://repo-default.voidlinux.org/live/current/",
        r'href="(void-live-x86_64-\d+-xfce\.iso)"')


def r_alpine():
    return latest_in_listing(
        "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/",
        r'href="(alpine-standard-[\d.]+-x86_64\.iso)"')


def r_mxlinux():
    return _sourceforge_rss("mx-linux", "/",
                            r"MX-[\d.]+(?:_[A-Za-z]+)?_x64\.iso")


def r_slackware():
    return _latest_subdir_iso(
        "https://mirrors.slackware.com/slackware/slackware-iso/",
        r'href="(slackware64-[\d.]+-install-dvd\.iso)"',
        dir_regex=r'href="(slackware64-[\d.]+-iso)/"')


# ---- server / enterprise ------------------------------------------------- #
def r_rocky():
    # 9.x uses "-dvd.iso", 10.x uses "-dvd1.iso"; match both.
    return _latest_subdir_iso(
        "https://download.rockylinux.org/pub/rocky/",
        r'href="(Rocky-[\d.]+-x86_64-dvd1?\.iso)"', sub="isos/x86_64/")


def r_alma():
    # repo.almalinux.org sends an incomplete cert chain (missing intermediate),
    # which Python won't chase -> use a mirror with a complete chain.
    return _latest_subdir_iso(
        "https://mirror.rackspace.com/almalinux/",
        r'href="(AlmaLinux-[\d.]+-x86_64-dvd\.iso)"', sub="isos/x86_64/")


def r_centos_stream():
    base = "https://mirror.stream.centos.org/"
    n = max(int(x) for x in re.findall(r'href="(\d+)-stream/"', http_get(base)))
    iso_base = "%s%d-stream/BaseOS/x86_64/iso/" % (base, n)
    return latest_in_listing(iso_base, r'href="(CentOS-Stream-[^"]+-dvd1\.iso)"')


# ---- security ------------------------------------------------------------ #
def r_parrot():
    return _latest_subdir_iso(
        "https://deb.parrot.sh/parrot/iso/",
        r'href="(Parrot-security-[\d.]+_amd64\.iso)"')


# ---- rescue / utility tools ---------------------------------------------- #
def r_rescuezilla():
    return _github_asset("rescuezilla/rescuezilla", r"64bit\.\w+\.iso$")


def r_supergrub2():
    return _sourceforge_rss("supergrub2", "/",
                            r"supergrub2-classic-[\d.a-z]+-multiarch-CD\.iso")


def r_rescatux():
    return _sourceforge_rss("rescatux", "/", r"rescatux[-_][\d.]+\.iso")


def r_bootrepair():
    return _sourceforge_rss("boot-repair-cd", "/",
                            r"boot-repair-disk-64bit\.iso")


def r_shredos():
    # Modern, actively maintained disk-wiper (nwipe) -- a DBAN successor.
    return _github_asset("PartialVolume/shredos.x86_64", r"x86-64.*\.iso$")


def r_finnix():
    return _latest_subdir_iso("https://www.finnix.org/releases/",
                              r'href="(finnix-[\d.]+\.iso)"')


def r_grml():
    # "full" ships the complete toolset; "small" is the stripped variant.
    return latest_in_listing("https://download.grml.org/",
                             r'href="(grml-full-[\d.]+-amd64\.iso)"')


def r_kaspersky():
    return ("https://rescuedisk.s.kaspersky-labs.com/updatable/2018/krd.iso",
            "krd.iso")


def r_eset():
    return ("https://download.eset.com/com/eset/tools/recovery/current/"
            "eset_sysrescue_live_enu.iso", "eset_sysrescue_live_enu.iso")


def r_ubcd():
    return ("https://mirror.koddos.net/ubcd/ubcd539.iso", "ubcd539.iso")


# ---- BSD / other --------------------------------------------------------- #
def r_freebsd():
    # download.freebsd.org sends an incomplete cert chain -> use a mirror with
    # a complete chain.
    return _latest_subdir_iso(
        "https://ftp.halifax.rwth-aachen.de/freebsd/releases/amd64/amd64/"
        "ISO-IMAGES/",
        r'href="(FreeBSD-[\d.]+-RELEASE-amd64-disc1\.iso)"')


# --------------------------------------------------------------------------- #
# Catalog
#
# Each entry: (key, label, category, tier, kind, payload)
#   tier    "standard" -> in Standard, Advanced, Everything
#           "advanced" -> in Advanced, Everything
#           "extra"    -> only in Everything
#   kind    "iso"      -> payload is the resolver function
#           "memtest"  -> payload is None (special zip->img handling)
#           "windows"  -> payload is a dict: win/rel/lang (for Fido) plus
#                         page/pick/tmpl (for the massgrave.dev fallback)
# Items keep their category order for the pickers.
# --------------------------------------------------------------------------- #
WIN11_PAGE = "https://massgrave.dev/windows_11_links"
WIN10_PAGE = "https://massgrave.dev/windows_10_links"
WINSERVER_PAGE = "https://massgrave.dev/windows-server-links"

CATALOG = [
    # ---- Desktop --------------------------------------------------------- #
    # Ordered by family (Arch-based, Ubuntu/Debian-based, Fedora, SUSE, Gentoo,
    # independents) so related entries sit next to each other in the picker.
    ("arch",          "Arch Linux",             "Desktop", "standard", "iso", r_arch),
    ("omarchy",       "Omarchy",                "Desktop", "standard", "iso", r_omarchy),
    ("endeavour",     "EndeavourOS",            "Desktop", "advanced", "iso", r_endeavouros),
    ("cachyos",       "CachyOS",                "Desktop", "advanced", "iso", r_cachyos),
    ("ubuntu_desk26", "Ubuntu 26.04 Desktop",   "Desktop", "standard", "iso", r_ubuntu_desktop_2604),
    ("ubuntu_desk24", "Ubuntu 24.04 Desktop",   "Desktop", "advanced", "iso", r_ubuntu_desktop_2404),
    ("mint",          "Linux Mint Cinnamon",    "Desktop", "standard", "iso", r_mint),
    ("mxlinux",       "MX Linux",               "Desktop", "advanced", "iso", r_mxlinux),
    ("debian_live",   "Debian Live GNOME",      "Desktop", "advanced", "iso", r_debian_live_gnome),
    ("fedora_kde",    "Fedora KDE Live",        "Desktop", "standard", "iso", r_fedora_kde),
    ("fedora_ws",     "Fedora Workstation",     "Desktop", "advanced", "iso", r_fedora_ws),
    ("opensuse_lp",   "openSUSE Leap",          "Desktop", "advanced", "iso", r_opensuse_leap),
    ("opensuse_tw",   "openSUSE Tumbleweed",    "Desktop", "advanced", "iso", r_opensuse_tumbleweed),
    ("gentoo_gui",    "Gentoo LiveGUI",         "Desktop", "advanced", "iso", r_gentoo_livegui),
    ("gentoo_min",    "Gentoo minimal",         "Desktop", "advanced", "iso", r_gentoo_minimal),
    ("nixos",         "NixOS graphical",        "Desktop", "standard", "iso", r_nixos_graphical),
    ("void",          "Void Linux (xfce)",      "Desktop", "advanced", "iso", r_void),
    ("alpine",        "Alpine Linux",           "Desktop", "advanced", "iso", r_alpine),
    ("slackware",     "Slackware64",            "Desktop", "extra",    "iso", r_slackware),
    # ---- Ubuntu flavours (same base, different desktop) ----------------- #
    ("kubuntu",       "Kubuntu",                "Ubuntu flavours", "advanced", "iso", _flavour("kubuntu")),
    ("xubuntu",       "Xubuntu",                "Ubuntu flavours", "advanced", "iso", _flavour("xubuntu")),
    ("lubuntu",       "Lubuntu",                "Ubuntu flavours", "advanced", "iso", _flavour("lubuntu")),
    ("ubuntu_mate",   "Ubuntu MATE",            "Ubuntu flavours", "extra",    "iso", _flavour("ubuntu-mate")),
    ("ubuntu_budgie", "Ubuntu Budgie",          "Ubuntu flavours", "extra",    "iso", _flavour("ubuntu-budgie")),
    ("ubuntu_cinn",   "Ubuntu Cinnamon",        "Ubuntu flavours", "extra",    "iso", _flavour("ubuntucinnamon")),
    ("ubuntu_studio", "Ubuntu Studio",          "Ubuntu flavours", "extra",    "iso", _flavour("ubuntustudio")),
    ("edubuntu",      "Edubuntu",               "Ubuntu flavours", "extra",    "iso", _flavour("edubuntu")),
    ("ubuntu_unity",  "Ubuntu Unity",           "Ubuntu flavours", "extra",    "iso", _flavour("ubuntu-unity")),
    ("ubuntu_kylin",  "Ubuntu Kylin",           "Ubuntu flavours", "extra",    "iso", _flavour("ubuntukylin")),
    # ---- Server / Enterprise ------------------------------------------- #
    ("debian",        "Debian netinst",         "Server",  "standard", "iso", r_debian),
    ("ubuntu_srv26",  "Ubuntu 26.04 Server",    "Server",  "standard", "iso", r_ubuntu_server_2604),
    ("ubuntu_srv24",  "Ubuntu 24.04 Server",    "Server",  "advanced", "iso", r_ubuntu_server_2404),
    ("proxmox",       "Proxmox VE",             "Server",  "standard", "iso", r_proxmox),
    ("pbs",           "Proxmox Backup Server",  "Server",  "advanced", "iso", r_proxmox_backup),
    ("pmg",           "Proxmox Mail Gateway",   "Server",  "extra",    "iso", r_proxmox_mailgw),
    ("pdm",           "Proxmox Datacenter Mgr", "Server",  "extra",    "iso", r_proxmox_dcm),
    ("xcpng",         "XCP-ng",                 "Server",  "advanced", "iso", r_xcpng),
    ("rocky",         "Rocky Linux",            "Server",  "advanced", "iso", r_rocky),
    ("alma",          "AlmaLinux",              "Server",  "advanced", "iso", r_alma),
    ("centos",        "CentOS Stream",          "Server",  "extra",    "iso", r_centos_stream),
    ("fedora_srv",    "Fedora Server",          "Server",  "extra",    "iso", r_fedora_server),
    # ---- Security ------------------------------------------------------- #
    ("kali",          "Kali netinst",           "Security","standard", "iso", r_kali),
    ("parrot",        "Parrot Security",        "Security","advanced", "iso", r_parrot),
    ("qubes",         "Qubes OS",               "Security","advanced", "iso", r_qubes),
    # ---- Rescue / Tools -------------------------------------------------- #
    # Grouped by job: repair system, partitioning/imaging, boot repair,
    # disk wiping, virus scan, hardware diagnostics.
    ("systemrescue",  "SystemRescue",           "Rescue",  "standard", "iso", r_systemrescue),
    ("hbcd",          "Hiren's BootCD PE",      "Rescue",  "standard", "iso", r_hbcd),
    ("finnix",        "Finnix (sysadmin)",      "Rescue",  "extra",    "iso", r_finnix),
    ("grml",          "Grml (sysadmin)",        "Rescue",  "advanced", "iso", r_grml),
    ("gparted",       "GParted Live",           "Rescue",  "standard", "iso", r_gparted),
    ("clonezilla",    "Clonezilla Live",        "Rescue",  "standard", "iso", r_clonezilla),
    ("rescuezilla",   "Rescuezilla",            "Rescue",  "advanced", "iso", r_rescuezilla),
    ("bootrepair",    "Boot-Repair-Disk",       "Rescue",  "advanced", "iso", r_bootrepair),
    ("supergrub2",    "Super Grub2 Disk",       "Rescue",  "advanced", "iso", r_supergrub2),
    ("rescatux",      "Rescatux",               "Rescue",  "extra",    "iso", r_rescatux),
    ("shredos",       "ShredOS (disk wiper)",   "Rescue",  "standard", "iso", r_shredos),
    ("dban",          "DBAN (final)",           "Rescue",  "extra",    "iso", r_dban),
    ("kaspersky",     "Kaspersky Rescue Disk",  "Rescue",  "extra",    "iso", r_kaspersky),
    ("eset",          "ESET SysRescue Live",    "Rescue",  "extra",    "iso", r_eset),
    ("memtest",       "MemTest86 (PassMark)",   "Rescue",  "standard", "memtest", None),
    ("ubcd",          "Ultimate Boot CD",       "Rescue",  "extra",    "iso", r_ubcd),
    # ---- BSD / other ---------------------------------------------------- #
    ("freebsd",       "FreeBSD",                "BSD/Other","extra",   "iso", r_freebsd),
    # ---- Windows -------------------------------------------------------- #
    # Tier "windows" is in no preset on purpose: these cannot be fetched
    # unattended, so they are offered through a separate opt-in prompt and
    # downloaded by hand from massgrave.dev in the browser.
    ("win11_de",      "Windows 11 German",      "Windows", "windows", "windows",
        {"win": "11", "rel": "25H2", "lang": "German", "page": WIN11_PAGE,
         "pick": "Win 11 Consumer 25H2 (Latest) -> German (de-de)",
         "tmpl": "Win_11_25H2_German_%s.iso"}),
    ("win11_en",      "Windows 11 English",     "Windows", "windows", "windows",
        {"win": "11", "rel": "25H2", "lang": "English", "page": WIN11_PAGE,
         "pick": "Win 11 Consumer 25H2 (Latest) -> English (en-us)",
         "tmpl": "Win_11_25H2_English_%s.iso"}),
    ("win10_en",      "Windows 10 English",     "Windows", "windows", "windows",
        {"win": "10", "rel": "22H2", "lang": "English", "page": WIN10_PAGE,
         "pick": "Win 10 22H2 (final) -> English",
         "tmpl": "Win_10_22H2_English_%s.iso"}),
    ("win10_de",      "Windows 10 German",      "Windows", "windows", "windows",
        {"win": "10", "rel": "22H2", "lang": "German", "page": WIN10_PAGE,
         "pick": "Win 10 22H2 (final) -> German",
         "tmpl": "Win_10_22H2_German_%s.iso"}),
    # ---- Windows Server ------------------------------------------------- #
    # Same opt-in path as the desktop editions (massgrave browser download).
    # "server": True marks them so process_windows keeps them on the massgrave
    # hand-off even under --windows fido/uup, neither of which covers Server.
    ("winsrv2025_en", "Windows Server 2025 English", "Windows Server",
        "windows", "windows",
        {"win": "2025", "rel": "2025", "lang": "English", "page": WINSERVER_PAGE,
         "pick": "Windows Server 2025 -> English (en-us)", "server": True,
         "tmpl": "Win_Server_2025_English_%s.iso"}),
    ("winsrv2025_de", "Windows Server 2025 German",  "Windows Server",
        "windows", "windows",
        {"win": "2025", "rel": "2025", "lang": "German", "page": WINSERVER_PAGE,
         "pick": "Windows Server 2025 -> German (de-de)", "server": True,
         "tmpl": "Win_Server_2025_German_%s.iso"}),
    ("winsrv2022_en", "Windows Server 2022 English", "Windows Server",
        "windows", "windows",
        {"win": "2022", "rel": "2022", "lang": "English", "page": WINSERVER_PAGE,
         "pick": "Windows Server 2022 -> English (en-us)", "server": True,
         "tmpl": "Win_Server_2022_English_%s.iso"}),
    ("winsrv2022_de", "Windows Server 2022 German",  "Windows Server",
        "windows", "windows",
        {"win": "2022", "rel": "2022", "lang": "German", "page": WINSERVER_PAGE,
         "pick": "Windows Server 2022 -> German (de-de)", "server": True,
         "tmpl": "Win_Server_2022_German_%s.iso"}),
]

# One-line "what is this and when do I boot it" for every catalog key.
# Shown in the picker so the list is readable without prior knowledge.
DESCRIPTIONS = {
    # Desktop -- same order and grouping as the catalog above
    "arch":          "rolling-release, build-it-yourself; text installer",
    "omarchy":       "preconfigured Arch + Hyprland desktop, ready to use",
    "endeavour":     "Arch made easy -- graphical installer, sane defaults",
    "cachyos":       "Blazingly Fast & Customizable Linux distribution",
    "ubuntu_desk26": "newest Ubuntu, easiest start for beginners",
    "ubuntu_desk24": "previous Ubuntu LTS, supported until 2029",
    "mint":          "beginner-friendly Ubuntu spin, classic Windows-like desktop",
    "mxlinux":       "lightweight Debian spin, runs well on old hardware",
    "debian_live":   "plain Debian as a live desktop -- try it before installing",
    "fedora_kde":    "modern KDE Plasma desktop, live-bootable",
    "fedora_ws":     "same Fedora with GNOME instead of KDE",
    "opensuse_lp":   "stable openSUSE release, enterprise-grade base",
    "opensuse_tw":   "the rolling openSUSE, always-newest packages",
    "gentoo_gui":    "Gentoo as a ready-made live desktop -- try before compiling",
    "gentoo_min":    "the same Gentoo as a text-only installer, built from source",
    "nixos":         "declarative OS -- whole system defined in one config file",
    "void":          "independent distro, runit instead of systemd, very lean",
    "alpine":        "tiny musl/busybox distro, popular for containers",
    "slackware":     "oldest surviving distro, minimal and unopinionated",
    # Ubuntu flavours -- identical Ubuntu base, different desktop/software set
    "kubuntu":       "Ubuntu with KDE Plasma; configurable, Windows-like",
    "xubuntu":       "Ubuntu with Xfce; light and classic, good on older PCs",
    "lubuntu":       "lightest Ubuntu (LXQt); revives weak/old hardware",
    "ubuntu_mate":   "Ubuntu with MATE, the traditional GNOME 2 desktop",
    "ubuntu_budgie": "Ubuntu with Budgie; tidy modern desktop, little fuss",
    "ubuntu_cinn":   "Ubuntu with Mint's Cinnamon desktop",
    "ubuntu_studio": "Ubuntu for audio/video/graphics work, low-latency kernel",
    "edubuntu":      "Ubuntu preloaded with educational software for schools",
    "ubuntu_unity":  "Ubuntu with the classic Unity 7 desktop",
    "ubuntu_kylin":  "official Ubuntu for Chinese users, UKUI desktop",
    # Server
    "debian":        "small net installer; rock-stable server base",
    "ubuntu_srv26":  "newest Ubuntu LTS server, no desktop",
    "ubuntu_srv24":  "previous Ubuntu LTS server, supported until 2029",
    "proxmox":       "bare-metal hypervisor for VMs and LXC containers",
    "pbs":           "backup server for Proxmox VE -- deduplicated, incremental",
    "pmg":           "mail gateway: spam and virus filter in front of a mail server",
    "pdm":           "single pane of glass for several Proxmox VE hosts",
    "xcpng":         "the other open hypervisor, XenServer-based; Xen Orchestra UI",
    "rocky":         "free RHEL clone for servers",
    "alma":          "free RHEL clone, alternative to Rocky",
    "centos":        "upstream preview of the next RHEL",
    "fedora_srv":    "Fedora as a server install, no desktop",
    # Security
    "kali":          "pentesting distro; net installer, pulls tools on install",
    "parrot":        "pentesting + privacy tools, lighter than Kali",
    "qubes":         "security by isolation -- every app in its own VM",
    # Rescue -- grouped by job, same order as the catalog above
    "systemrescue":  "Arch-based repair system; partitioning, data recovery",
    "hbcd":          "Windows PE toolkit; repair and diagnose Windows offline",
    "finnix":        "small text-only live system for sysadmin work",
    "grml":          "Debian-based sysadmin live system, heavy on shell tooling",
    "gparted":       "graphical partition editor; resize/move/format disks",
    "clonezilla":    "disk and partition imaging, clone drives 1:1",
    "rescuezilla":   "the same imaging with a friendly GUI, reads Clonezilla images",
    "bootrepair":    "one-click repair for GRUB/EFI boot problems",
    "supergrub2":    "boots a system whose bootloader is broken, without fixing it",
    "rescatux":      "boot repair plus Windows/Linux password reset",
    "shredos":       "secure disk eraser, handles SSD/NVMe properly",
    "dban":          "its ancient predecessor, HDD only -- kept for old hardware",
    "kaspersky":     "offline virus scan for an infected Windows install",
    "eset":          "offline virus scan, second opinion to Kaspersky",
    "memtest":       "RAM test, finds faulty memory modules",
    "ubcd":          "collection of DOS-era hardware diagnostic tools",
    # BSD / other
    "freebsd":       "not Linux -- the BSD server/NAS operating system",
    # Windows (manual, via massgrave.dev)
    "win11_de":      "official Win 11 25H2 installer, German",
    "win11_en":      "official Win 11 25H2 installer, English",
    "win10_en":      "official Win 10 22H2 installer, English",
    "win10_de":      "official Win 10 22H2 installer, German",
    # Windows Server (manual, via massgrave.dev)
    "winsrv2025_en": "Windows Server 2025 -- current LTSC, English",
    "winsrv2025_de": "Windows Server 2025 -- current LTSC, German",
    "winsrv2022_en": "Windows Server 2022 -- previous LTSC, English",
    "winsrv2022_de": "Windows Server 2022 -- previous LTSC, German",
}

# Windows is deliberately absent from every preset -- see choose_windows().
TIERS = {"standard": {"standard"},
         "advanced": {"standard", "advanced"},
         "everything": {"standard", "advanced", "extra"}}


def preset_items(preset):
    tiers = TIERS[preset]
    return [it for it in CATALOG if it[3] in tiers]


# --------------------------------------------------------------------------- #
# Downloader
# --------------------------------------------------------------------------- #
def _progress(done, total, bps, final=False):
    """Render one progress line: %, sizes, and live speed in MB/s + Mbit/s."""
    mbps = bps / 1e6          # decimal megabytes / s
    mbit = bps * 8 / 1e6      # megabits / s  (== MB/s * 8)
    if total:
        pct = done * 100.0 / total
        line = "    %5.1f%%  %s / %s  @ %6.2f MB/s (%6.1f Mbit/s)" % (
            pct, _fmt_size(done), _fmt_size(total), mbps, mbit)
    else:
        line = "    %s  @ %6.2f MB/s (%6.1f Mbit/s)" % (
            _fmt_size(done), mbps, mbit)
    # pad to clear any leftover characters from a longer previous line
    sys.stdout.write("\r" + line + " " * 6)
    sys.stdout.flush()
    if final:
        sys.stdout.write("\n")
        sys.stdout.flush()


def download(url, dest, sha256=None):
    """Download url -> dest safely, showing the serving host and live speed.

    Writes to `dest.part` and only atomically renames onto `dest` once the
    transfer is verified, so an existing good file is never clobbered by a
    truncated or wrong-content (e.g. HTML error page) download. Resumes an
    interrupted `.part` via an HTTP Range request; a 200 (range ignored)
    restarts from scratch, a 416 means the .part is already complete.

    With `sha256` the finished .part is hashed before the rename, so a corrupted
    or tampered image never reaches the final name -- size alone would not catch
    it. A mismatch deletes the .part: resuming it would only rebuild the same
    bad file."""
    part = dest + ".part"
    resume_from = os.path.getsize(part) if os.path.exists(part) else 0
    headers = {"User-Agent": _ua_for(url)}
    if resume_from:
        headers["Range"] = "bytes=%d-" % resume_from
    req = urllib.request.Request(url, headers=headers)

    try:
        r = urllib.request.urlopen(req, timeout=60, context=CTX)
    except urllib.error.HTTPError as e:
        if e.code == 416 and resume_from:      # .part already has it all
            os.replace(part, dest)
            print("    already complete")
            return
        raise

    with r:
        host = urlparse(r.geturl()).netloc or urlparse(url).netloc
        ctype = r.headers.get("Content-Type", "")
        if "html" in ctype.lower():
            raise RuntimeError("server returned an HTML page, not a file "
                               "(%s)" % host)
        if r.getcode() == 206 and resume_from:
            done = resume_from
            cr = r.headers.get("Content-Range", "")
            m = re.search(r"/(\d+)\s*$", cr)
            total = int(m.group(1)) if m else None
            mode = "ab"
            print("    server: %s  (resuming at %s)" % (host, _fmt_size(done)))
        else:
            done = resume_from = 0
            cl = r.headers.get("Content-Length")
            total = int(cl) if cl else None
            mode = "wb"
            print("    server: %s" % host)

        start = last_t = time.monotonic()
        last_b = done
        with open(part, mode) as f:
            while True:
                chunk = r.read(256 * 1024)
                if not chunk:
                    break
                f.write(chunk)
                done += len(chunk)
                now = time.monotonic()
                if now - last_t >= 0.5:
                    _progress(done, total, (done - last_b) / (now - last_t))
                    last_t, last_b = now, done
        elapsed = max(time.monotonic() - start, 1e-6)
        _progress(done, total, (done - resume_from) / elapsed, final=True)

    got = os.path.getsize(part)
    if total is not None and got != total:
        raise RuntimeError("incomplete download: got %s of %s"
                           % (_fmt_size(got), _fmt_size(total)))
    if sha256:
        sys.stdout.write("    verifying sha256 ...")
        sys.stdout.flush()
        actual = sha256_file(part)
        if actual != sha256:
            os.remove(part)
            raise RuntimeError("sha256 mismatch (expected %s..., got %s...)"
                               % (sha256[:12], actual[:12]))
        sys.stdout.write(" ok\n")
    os.replace(part, dest)


def move_with_progress(src, dst, chunk=4 * 1024 * 1024):
    """Move a file, showing a live progress line whenever it has to be copied.

    Moving onto the stick crosses filesystems, so shutil.move() degrades to a
    silent copy -- with a multi-GB Windows ISO on a slow USB port that is
    minutes of a frozen-looking screen. A same-volume move is still an instant
    rename; only the real copy prints anything, and it goes through a .part
    file so an interrupted copy can never leave half an ISO under the final
    name."""
    try:
        os.rename(src, dst)
        return
    except OSError:
        pass                              # different volume -> copy it over

    total = os.path.getsize(src)
    part = dst + ".part"
    print("    copying %s to %s" % (_fmt_size(total), os.path.dirname(dst)))
    done = 0
    start = last_t = time.monotonic()
    last_b = 0
    try:
        with open(src, "rb") as fi, open(part, "wb") as fo:
            while True:
                buf = fi.read(chunk)
                if not buf:
                    break
                fo.write(buf)
                done += len(buf)
                now = time.monotonic()
                if now - last_t >= 0.5:
                    _progress(done, total, (done - last_b) / (now - last_t))
                    last_t, last_b = now, done
        _progress(done, total, done / max(time.monotonic() - start, 1e-6),
                  final=True)
        if done != total:
            raise RuntimeError("incomplete copy: %s of %s"
                               % (_fmt_size(done), _fmt_size(total)))
        os.replace(part, dst)
    except BaseException:
        # Ctrl+C included: a stray .part on the stick is pure confusion.
        if os.path.exists(part):
            try:
                os.remove(part)
            except OSError:
                pass
        raise
    os.remove(src)


def download_memtest(dest_dir):
    """PassMark MemTest86 free: download the zip, extract the .img."""
    tmp = tempfile.mkdtemp(prefix="memtest_")
    zpath = os.path.join(tmp, "memtest86-usb.zip")
    download("https://www.memtest86.com/downloads/memtest86-usb.zip", zpath)
    with zipfile.ZipFile(zpath) as z:
        img = next(n for n in z.namelist() if n.lower().endswith(".img"))
        z.extract(img, tmp)
        shutil.copy(os.path.join(tmp, img),
                    os.path.join(dest_dir, "memtest86-usb.img"))
    shutil.rmtree(tmp, ignore_errors=True)


# --------------------------------------------------------------------------- #
# Version manifest  (records what is currently on the stick)
# --------------------------------------------------------------------------- #
def load_manifest(dest):
    path = os.path.join(dest, MANIFEST_NAME)
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return {}


def save_manifest(dest, manifest):
    path = os.path.join(dest, MANIFEST_NAME)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2, ensure_ascii=False, sort_keys=True)
    os.replace(tmp, path)


def check_update(dest, entry, url, name):
    """Decide whether `name` needs (re)downloading.

    Returns (needs_update, reason, meta) where meta is (size, mtime) or None.
    Handles three cases, and in two of them a file that is already correct is
    adopted rather than fetched again:

      * a manifest record under the same name -- the fast rolling-release check
      * a record under a different name, yet the new name is already on disk:
        a download that completed but was never recorded
      * no record at all, e.g. an ISO put there by hand

    In the last two the local size is compared against the remote one; a match
    adopts the file, a mismatch redownloads, and an unreadable remote size
    leaves the file alone rather than destroying it on a guess.
    """
    path = os.path.join(dest, name)
    if not os.path.exists(path):
        return True, "not on stick", None
    local = os.path.getsize(path)

    if entry and entry.get("filename") == name:
        size, mtime = remote_meta(url)
        if size and entry.get("size") and int(size) != int(entry["size"]):
            return True, "remote size changed", (size, mtime)
        # Last-Modified alone is not trustworthy: mirrors and CDNs hand out
        # different timestamps for byte-identical files, which would trigger
        # pointless multi-GB re-downloads. Only trust it when there is no size.
        if (not size and mtime and entry.get("mtime")
                and mtime != entry["mtime"]):
            return True, "remote updated", (size, mtime)
        return False, "up to date", (size, mtime)

    if entry and entry.get("filename") != name:
        # The file under the *new* name is already here -- a missing one
        # returned above. So a previous run downloaded it and never got to
        # record it: stick pulled, Ctrl+C, write error. Adopt it if it matches
        # the remote size instead of pulling several GB down again.
        size, mtime = remote_meta(url)
        if size and local == int(size):
            return False, "present, adopted (was %s)" % entry.get("filename"), \
                (size, mtime)
        return True, "new version %s -> %s" % (entry.get("filename"), name), \
            (size, mtime)

    # Present on the stick but not in the manifest -> adopt or refresh by size.
    size, mtime = remote_meta(url)
    if size and local == int(size):
        return False, "present, adopted", (size, mtime)
    if size and local != int(size):
        return True, "size mismatch -> redownload", (size, mtime)
    return False, "present (size unverifiable)", (size, mtime)


HISTORY_KEEP = 5      # former filenames remembered per item


def record_version(dest, manifest, label, url, name, meta):
    path = os.path.join(dest, name)
    size = meta[0] if meta and meta[0] else (
        os.path.getsize(path) if os.path.exists(path) else None)
    mtime = meta[1] if meta else None
    # Remember the filenames this item used to have. Version bumps that also
    # change the naming scheme (an upstream rename, a fixed-up name here) are
    # otherwise untraceable, and the leftover file would sit on the stick
    # forever -- see find_superseded().
    prev = manifest.get(label) or {}
    history = [h for h in (prev.get("history") or []) if h != name]
    old = prev.get("filename")
    if old and old != name and old not in history:
        history.append(old)
    manifest[label] = {"filename": name, "url": url,
                       "size": size, "mtime": mtime}
    if history:
        manifest[label]["history"] = history[-HISTORY_KEEP:]
    save_manifest(dest, manifest)


def fetch_tracked(dest, manifest, key, label, url, name, mirrors, args, summary):
    """Update-check -> (mirror race) -> checksum -> download -> record + prune.
    Shared by the Linux ISO loop and the Windows/Fido path. Under --dry-run it
    stops after the check and reports what it would have done."""
    entry = manifest.get(key)
    if args.force:
        need, reason, meta = True, "forced", None
    else:
        need, reason, meta = check_update(dest, entry, url, name)
    print("==> %-24s %s  [%s]" % (label, name, reason))
    if args.dry_run:
        # Nothing is written in a dry run, manifest included -- the whole point
        # is to see what a real run would change.
        summary.append((name, "WOULD DOWNLOAD" if need else "SKIP (up to date)"))
        print()
        return
    if not need:
        # Persist the current on-disk state so later runs skip instantly, even
        # for files that were already on the stick (adopted, no prior record).
        if not entry or entry.get("filename") != name:
            record_version(dest, manifest, key, url, name, meta)
        # Drop a leftover partial from an aborted download -- the real file is
        # current, so the fragment is just wasted space.
        part = os.path.join(dest, name + ".part")
        if os.path.exists(part):
            try:
                stale = _fmt_size(os.path.getsize(part))
                os.remove(part)
                print("    removed stale partial (%s)" % stale)
            except OSError:
                pass
        summary.append((name, "SKIP (up to date)"))
        print()
        return
    try:
        if mirrors and not args.no_mirror_test:
            url = fastest_mirror([url] + mirrors)
        # Resolve the checksum only after the mirror is settled, so the sums
        # file comes from the very host that serves the image.
        expected = None if args.no_verify else remote_sha256(url, name)
        if not expected and not args.no_verify:
            print("    no published checksum -- size check only")
        download(url, os.path.join(dest, name), expected)
        old = entry.get("filename") if entry else None
        if old and old != name:
            old_path = os.path.join(dest, old)
            if os.path.exists(old_path):
                os.remove(old_path)
                print("    removed old %s" % old)
        gb = os.path.getsize(os.path.join(dest, name)) / (1024 ** 3)
        print("    ok  (%.2f GB)\n" % gb)
        record_version(dest, manifest, key, url, name, meta)
        summary.append((name, "OK (updated)"))
    except Exception as e:
        print("    FAILED: %s\n" % e)
        summary.append((name, "FAIL: %s" % e))


# --------------------------------------------------------------------------- #
# Cleanup: superseded ISOs left on the stick
#
# fetch_tracked() only deletes the file the manifest recorded for that exact
# item. Anything the manifest lost track of -- an ISO from before the manifest
# existed, one downloaded by hand, one left behind by an aborted run -- stays
# on the stick forever. This sweep finds those leftovers, but only when it can
# name the newer file that replaced them.
# --------------------------------------------------------------------------- #
SWEEP_EXTS = (".iso", ".img")


def _family_regex(name):
    r"""Regex matching other versions of the same ISO, capturing the numbers.

    Every run of digits/dots in the stem becomes a capture group, everything
    else stays literal: "omarchy-3.8.4.iso" -> "omarchy-([\d.]+)\.iso". That
    matches 3.8.3 but not "omarchy-dev.iso", and -- unlike a plain prefix
    match -- not a different product that merely starts with the same word
    (ubuntu-...-desktop vs ubuntu-...-server)."""
    stem, ext = os.path.splitext(name)
    parts = re.split(r"([\d.]+)", stem)
    body = "".join(r"([\d.]+)" if i % 2 else re.escape(p)
                   for i, p in enumerate(parts))
    return re.compile(r"^%s%s$" % (body, re.escape(ext)), re.I)


def _version_tokens(rgx, name):
    """Per-position version numbers of `name` within its family, or None.

    Comparing these instead of every integer in the filename keeps constants
    out of the comparison: "ubuntu-24.04-..-amd64" vs "ubuntu-24.04.3-..-
    amd64" must hinge on 24.04 < 24.04.3, not on the amd64 that follows it."""
    m = rgx.match(name)
    return None if m is None else tuple(verkey(g) for g in m.groups())


def _shared_prefix(a, b):
    """How many leading version numbers two token tuples agree on -- used to
    pick the closest relative when several items claim the same file."""
    n = 0
    for ta, tb in zip(a, b):
        for x, y in zip(ta, tb):
            if x != y:
                return n
            n += 1
    return n


def current_files(manifest, resolved, extra=()):
    """key -> (label, filename that *should* be on the stick) for everything
    we know about: this run's resolved items plus every manifest record (so
    items not selected this run are still recognized, not swept)."""
    labels = {it[0]: it[1] for it in CATALOG}
    cur = {}
    for key, entry in manifest.items():
        name = entry.get("filename") if isinstance(entry, dict) else None
        if name:
            cur[key] = (labels.get(key, key), name)
    for key, label, _url, name, _mirrors in resolved:
        cur[key] = (label, name)
    for key, label, name in extra:
        cur[key] = (label, name)
    return cur


def find_superseded(dest, manifest, current):
    """[(label, old_filename, size, new_filename)] of replaced ISOs on disk.

    A file is only reported when exactly one item claims it -- either because
    the manifest recorded it under that item before a rename, or because it
    matches that item's filename family *and* carries a lower version number.
    Ambiguous matches, newer files and anything currently in use are left
    alone: this deletes multi-GB downloads, so it errs towards keeping."""
    protected = {name.lower() for _label, name in current.values()}
    protected.add(MANIFEST_NAME.lower())
    history = {k: {h.lower() for h in (e.get("history") or [])}
               for k, e in manifest.items() if isinstance(e, dict)}
    families = {k: _family_regex(name) for k, (_l, name) in current.items()}

    found = []
    try:
        listing = sorted(os.listdir(dest))
    except OSError:
        return found
    for fn in listing:
        # A ".part" belonging to a superseded ISO is dead weight too, but one
        # belonging to a current file is a resumable download -- keep it.
        base = fn[:-len(".part")] if fn.lower().endswith(".part") else fn
        if not base.lower().endswith(SWEEP_EXTS):
            continue
        if base.lower() in protected:
            continue
        path = os.path.join(dest, fn)
        if not os.path.isfile(path):
            continue
        owners = [k for k, seen in history.items() if base.lower() in seen]
        if not owners:
            # Every item whose family matches and whose current file is newer.
            # Several can match (Ubuntu 24.04 vs 26.04 desktop); the closest
            # version wins, and a genuine tie is left alone.
            claims = []
            for k, rgx in families.items():
                tok = _version_tokens(rgx, base)
                cur_tok = _version_tokens(rgx, current[k][1])
                if tok is None or cur_tok is None or tok >= cur_tok:
                    continue
                claims.append((_shared_prefix(tok, cur_tok), k))
            if claims:
                best = max(score for score, _k in claims)
                owners = [k for score, k in claims if score == best]
        if len(owners) != 1:
            continue
        label, newer = current[owners[0]]
        try:
            size = os.path.getsize(path)
        except OSError:
            size = 0
        found.append((label, fn, size, newer))
    return found


def cleanup_superseded(dest, manifest, current, mode, summary):
    """List superseded ISOs and, once confirmed, delete them."""
    old = find_superseded(dest, manifest, current)
    if not old:
        return
    total = sum(size for _l, _n, size, _new in old)
    width = max(len(n) for _l, n, _s, _new in old)

    print("\n" + "-" * 15 + " OLD VERSIONS " + "-" * 15)
    print("Superseded by a newer release, still taking up space:\n")
    for label, name, size, newer in old:
        print("  %s" % label)
        print("    old: %-*s  %s" % (width, name, _fmt_size(size)))
        print("    new: %s" % newer)
    print("\n  %d file(s), %s total" % (len(old), _fmt_size(total)))

    if mode == "no":
        # Also the dry-run path, which forces this mode -- so word it for both
        # instead of naming a flag the user may not have passed.
        print("  (keeping them -- pass --cleanup yes to delete)")
        return
    if mode == "ask":
        try:
            ans = input("\nDelete these %d file(s)? [y/N]: "
                        % len(old)).strip().lower()
        except EOFError:
            ans = ""
        if ans not in ("y", "yes", "j", "ja"):
            print("Keeping them.")
            return

    print()
    freed = 0
    for label, name, size, _newer in old:
        try:
            os.remove(os.path.join(dest, name))
            freed += size
            print("  deleted %s" % name)
            summary.append((name, "DELETED (old version of %s)" % label))
        except OSError as e:
            print("  could NOT delete %s: %s" % (name, e))
            summary.append((name, "DELETE FAILED: %s" % e))
    print("\nFreed %s." % _fmt_size(freed))


def stick_inventory(dest, manifest):
    """What is on the stick right now: (installed, others).

    `installed` maps catalog key -> (filename, size) for every manifest record
    whose file is really there; `others` lists (filename, size) of images the
    manifest doesn't account for. Purely local -- no network, so the pickers
    can show it before anything is resolved."""
    installed, claimed = {}, set()
    for key, entry in manifest.items():
        name = entry.get("filename") if isinstance(entry, dict) else None
        if not name:
            continue
        claimed.add(name.lower())
        try:
            installed[key] = (name, os.path.getsize(os.path.join(dest, name)))
        except OSError:
            pass                        # recorded but gone -> not installed
    others = []
    try:
        listing = sorted(os.listdir(dest))
    except OSError:
        listing = []
    for fn in listing:
        if not fn.lower().endswith(SWEEP_EXTS) or fn.lower() in claimed:
            continue
        try:
            others.append((fn, os.path.getsize(os.path.join(dest, fn))))
        except OSError:
            pass
    return installed, others


def print_inventory(dest, installed, others, numbers=None):
    """Show what the stick already holds, so nothing gets picked twice.

    `numbers` maps key -> catalog number; when given, each line starts with
    the number you would type to add or drop that item."""
    total = (sum(s for _n, s in installed.values())
             + sum(s for _n, s in others))
    if not installed and not others:
        print("\nNothing on %s yet." % dest)
        return
    print("\nAlready on %s -- %d item(s), %s:\n"
          % (dest, len(installed) + len(others), _fmt_size(total)))
    labels = {it[0]: it[1] for it in CATALOG}
    order = {it[0]: i for i, it in enumerate(CATALOG)}
    for key in sorted(installed, key=lambda k: order.get(k, 10 ** 6)):
        name, size = installed[key]
        num = "%3s" % (numbers.get(key, "") if numbers else "")
        print("  %s  %-24s %-42s %9s"
              % (num, labels.get(key, key), name, _fmt_size(size)))
    for name, size in others:
        print("  %s  %-24s %-42s %9s" % ("   ", "(not in the catalog)",
                                         name, _fmt_size(size)))


# --------------------------------------------------------------------------- #
# USB-drive detection + interactive target picker
# --------------------------------------------------------------------------- #
def _win_volume_label(root):
    import ctypes
    buf = ctypes.create_unicode_buffer(261)
    fsbuf = ctypes.create_unicode_buffer(261)
    try:
        ctypes.windll.kernel32.GetVolumeInformationW(
            ctypes.c_wchar_p(root), buf, ctypes.sizeof(buf),
            None, None, None, fsbuf, ctypes.sizeof(fsbuf))
    except Exception:
        return ""
    return buf.value


def _mount_total(path):
    try:
        return shutil.disk_usage(path).total
    except Exception:
        return 0


def _is_wsl():
    """True inside WSL, where the Windows drives appear as drvfs under /mnt."""
    if "WSL_DISTRO_NAME" in os.environ or "WSL_INTEROP" in os.environ:
        return True
    try:
        with open("/proc/version") as fh:
            return "microsoft" in fh.read().lower()
    except OSError:
        return False


def _proc_mounts():
    """[(device, mountpoint, fstype)] from /proc/mounts.

    Mountpoints are escaped in that file -- a space is written "\\040" -- so
    the octal escapes have to be decoded before the path is usable.
    """
    entries = []
    try:
        with open("/proc/mounts") as fh:
            raw = fh.read()
    except OSError:
        return entries
    for line in raw.splitlines():
        parts = line.split()
        if len(parts) < 3:
            continue
        mnt = re.sub(r"\\([0-7]{3})", lambda m: chr(int(m.group(1), 8)), parts[1])
        entries.append((parts[0], mnt, parts[2]))
    return entries


def _block_is_removable(device):
    """Ask /sys whether the disk behind a /dev node is removable.

    Partitions carry no removable flag of their own, so walk up from the
    partition to the whole-disk name by trimming characters until /sys/block
    knows it: sdb1 -> sdb, nvme0n1p2 -> nvme0n1, mmcblk0p1 -> mmcblk0.
    """
    name = os.path.basename(device)
    while name and not os.path.isdir("/sys/block/%s" % name):
        name = name[:-1]
    if not name:
        return False
    try:
        with open("/sys/block/%s/removable" % name) as fh:
            return fh.read().strip() == "1"
    except OSError:
        return False


def _wsl_windows_removable():
    """[(letter, label, size)] of removable Windows drives, asked from Windows.

    Under WSL a stick is a drvfs mount, so /sys has no block device to check
    and nothing on the Linux side distinguishes it from the system drive. The
    only source of truth is Windows itself.
    """
    cmd = ("Get-CimInstance Win32_LogicalDisk | "
           "Where-Object { $_.DriveType -eq 2 } | "
           "ForEach-Object { \"$($_.DeviceID)|$($_.VolumeName)|$($_.Size)\" }")
    try:
        out = subprocess.check_output(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", cmd],
            stderr=subprocess.DEVNULL, timeout=25)
    except Exception:
        return []
    found = []
    for line in out.decode("utf-8", "replace").splitlines():
        parts = line.strip().split("|")
        if len(parts) != 3 or not parts[0][:1].isalpha():
            continue
        found.append((parts[0][0].lower(), parts[1] or "(no label)",
                      int(parts[2]) if parts[2].isdigit() else 0))
    return found


def _wsl_drvfs_candidates():
    """Windows drives under /mnt/<letter>, used when interop is unavailable.

    Asking Windows is exact but needs .exe interop, and some setups do not have
    it -- notably Arch images booting systemd, where systemd-binfmt drops WSL's
    binfmt registration and every .exe fails with "Exec format error". Fall
    back to the drvfs mounts themselves and label them as unverified, because
    nothing on this side says which drive is removable.

    /mnt/c is skipped on purpose: the picker treats the first entry as the
    default, and the system drive is the one answer that must never be it.
    """
    found = []
    for _device, mnt, fs in _proc_mounts():
        if fs not in ("drvfs", "9p", "virtiofs"):
            continue
        match = re.match(r"^/mnt/([a-z])$", mnt)
        if not match or match.group(1) == "c":
            continue
        found.append((mnt,
                      "%s: windows drive (unverified)" % match.group(1).upper(),
                      _mount_total(mnt)))
    return found


def wsl_unmounted_drives():
    """[(letter, label)] of removable Windows drives WSL has not mounted.

    WSL only automounts what existed when it started, so a stick plugged in
    afterwards stays invisible until it is mounted by hand.
    """
    if not _is_wsl():
        return []
    return [(letter, label) for letter, label, _size in _wsl_windows_removable()
            if not os.path.ismount("/mnt/%s" % letter)]


def detect_usb_drives():
    """Return [(path, label, total_bytes)] of removable drives, best-effort."""
    drives = []
    if os.name == "nt":
        import ctypes
        import string
        k32 = ctypes.windll.kernel32
        bitmask = k32.GetLogicalDrives()
        for i, letter in enumerate(string.ascii_uppercase):
            if not (bitmask >> i) & 1:
                continue
            root = "%s:\\" % letter
            if k32.GetDriveTypeW(ctypes.c_wchar_p(root)) != 2:  # DRIVE_REMOVABLE
                continue
            try:
                total = shutil.disk_usage(root).total
            except Exception:
                total = 0
            drives.append((root, _win_volume_label(root) or "(no label)", total))
        return drives

    seen = set()

    # Linux: /proc/mounts plus the removable flag in /sys is authoritative and,
    # unlike scanning fixed directories, does not care where the desktop
    # environment decided to mount the stick -- or whether one mounted it at
    # all and it sits somewhere like /mnt/stick.
    for device, mnt, _fs in _proc_mounts():
        if not device.startswith("/dev/") or mnt in seen:
            continue
        if not _block_is_removable(device):
            continue
        seen.add(mnt)
        drives.append((mnt, os.path.basename(mnt) or mnt, _mount_total(mnt)))

    # WSL: those are drvfs mounts with no block device behind them, so they
    # never show up above. Ask Windows which of them is removable.
    if _is_wsl():
        exact = [("/mnt/%s" % letter, label, size)
                 for letter, label, size in _wsl_windows_removable()]
        for path, label, size in exact or _wsl_drvfs_candidates():
            if path in seen or not os.path.ismount(path):
                continue
            seen.add(path)
            drives.append((path, label, size or _mount_total(path)))

    # macOS has no /sys and mounts removable volumes under /Volumes, so keep
    # the directory scan for it -- and as a fallback for anything the checks
    # above did not describe.
    user = os.environ.get("USER") or os.environ.get("LOGNAME") or ""
    for base in ("/Volumes", os.path.join("/media", user), "/media",
                 os.path.join("/run/media", user)):
        if not os.path.isdir(base):
            continue
        for entry in sorted(os.listdir(base)):
            path = os.path.join(base, entry)
            if path in seen or not os.path.ismount(path):
                continue
            seen.add(path)
            drives.append((path, entry, _mount_total(path)))
    return drives


def _quit():
    """Leave cleanly from an interactive menu -- nothing has been written yet."""
    print("Nothing written.")
    sys.exit(0)


def choose_dest():
    """Interactive target picker used when --dest is not supplied."""
    drives = detect_usb_drives()
    print("Where should the ISOs go?\n")
    for i, (path, label, total) in enumerate(drives, 1):
        size = _fmt_size(total) if total else "?"
        print("  [%d] %s  %-20s  %s" % (i, path, label, size))
    if not drives:
        print("  (no removable USB drives detected)")
    # WSL automounts only what was plugged in when it started, so a stick added
    # later is invisible until it is mounted -- say so instead of letting the
    # user conclude the drive is broken.
    for letter, label in wsl_unmounted_drives():
        print("  note: Windows drive %s: (%s) is not mounted in WSL yet:"
              % (letter.upper(), label))
        print("        sudo mkdir -p /mnt/%s && sudo mount -t drvfs %s: /mnt/%s"
              % (letter, letter.upper(), letter))
    print("  [o] other path (type it in)")
    print("  [.] default folder ./isos")
    print("  [q] quit\n")

    # Default hint tracks what an empty answer actually does: the first drive
    # when one exists, otherwise the ./isos folder -- there is no [1] to offer.
    default = "1" if drives else "."
    try:
        choice = input("Select target [%s]: " % default).strip().lower()
    except EOFError:
        choice = ""

    if choice == "q":
        _quit()
    if choice == "" and drives:
        return drives[0][0]
    if choice == "" or choice == ".":
        return "./isos"
    if choice == "o":
        try:
            manual = input("Path: ").strip().strip('"')
        except EOFError:
            manual = ""
        return manual or "./isos"
    if choice.isdigit() and 1 <= int(choice) <= len(drives):
        return drives[int(choice) - 1][0]
    print("Unrecognized choice -> using ./isos")
    return "./isos"


# --------------------------------------------------------------------------- #
# ISO selection  (presets + individual custom picker)
# --------------------------------------------------------------------------- #
def parse_ranges(raw, n):
    """Turn '1,3,5-9' into a sorted list of valid 1-based indices."""
    picks = set()
    for part in raw.replace(" ", "").split(","):
        if not part:
            continue
        if "-" in part:
            a, _, b = part.partition("-")
            if a.isdigit() and b.isdigit():
                for i in range(int(a), int(b) + 1):
                    if 1 <= i <= n:
                        picks.add(i)
        elif part.isdigit() and 1 <= int(part) <= n:
            picks.add(int(part))
    return sorted(picks)


def custom_select(preselect=None, dest=None, manifest=None):
    """Numbered, category-grouped picker. Returns catalog items in order.

    `preselect` is a set of keys shown as [x] so the user can start from a
    preset and only adjust it. With `dest`/`manifest` the picker first lists
    what the stick already holds and marks those entries with a '*', so it is
    obvious what would be a re-download and what is missing."""
    preselect = preselect or set()
    numbers = {it[0]: i for i, it in enumerate(CATALOG, 1)}
    installed = {}
    if dest is not None and manifest is not None:
        installed, others = stick_inventory(dest, manifest)
        print_inventory(dest, installed, others, numbers)

    print("\nPick items individually. Enter numbers/ranges, e.g. 1,3,5-9.")
    print("Prefix with '-' to remove from the preset, '+' to add. "
          "'a'=all, ''=keep preset, 'q'=quit.")
    print("  [x] = selected    * = already on the stick "
          "(re-picking it only checks for updates)")
    last = None
    for i, it in enumerate(CATALOG, 1):
        if it[2] != last:
            print("\n  == %s ==" % it[2])
            last = it[2]
        mark = "x" if it[0] in preselect else " "
        have = "*" if it[0] in installed else " "
        print("   [%s] %s %2d  %-24s %s"
              % (mark, have, i, it[1], DESCRIPTIONS.get(it[0], "")))

    try:
        raw = input("\nSelection: ").strip().lower()
    except EOFError:
        raw = ""

    if raw == "q":
        _quit()
    chosen = {it[0] for it in CATALOG if it[0] in preselect}
    if raw in ("a", "all"):
        return list(CATALOG)
    if raw == "":
        return [it for it in CATALOG if it[0] in chosen]
    if raw[0] in "+-":
        # incremental edits against the preset
        for token in re.findall(r"[+-][\d,\-]+", raw):
            op, body = token[0], token[1:]
            keys = {CATALOG[i - 1][0] for i in parse_ranges(body, len(CATALOG))}
            chosen = (chosen | keys) if op == "+" else (chosen - keys)
        return [it for it in CATALOG if it[0] in chosen]
    # plain list -> exact selection
    idx = parse_ranges(raw, len(CATALOG))
    return [CATALOG[i - 1] for i in idx]


def choose_selection(preset_arg=None, dest=None, manifest=None):
    """Top-level 'Standard / Advanced / Everything / Custom' menu.
    Returns the list of selected catalog items (in catalog order)."""
    if preset_arg in TIERS:
        return preset_items(preset_arg)
    if preset_arg == "custom":
        return custom_select(dest=dest, manifest=manifest)

    installed = {}
    if dest is not None and manifest is not None:
        installed = stick_inventory(dest, manifest)[0]

    std, adv, alln = (len(preset_items("standard")),
                      len(preset_items("advanced")),
                      len(preset_items("everything")))
    print("\nWhat should go on the stick?\n")
    print("  [1] Standard    (%2d items)  one solid pick per job" % std)
    print("  [2] Advanced    (%2d items)  + sibling releases & more tools" % adv)
    print("  [3] Everything  (%2d items)  + niche and legacy items" % alln)
    print("  [4] Custom                 pick individually")
    print("  [5] Custom from Standard   start with Standard, then adjust")
    print("  [q] quit")
    if installed:
        print("  [6] Update what's there (%2d items)  keep the current set, "
              "just refresh it" % len(installed))
        print("\n  -> Been here before? Take [6] first: it brings the stick "
              "up to date without\n     adding anything new. Only picking a "
              "preset on a stick this script has\n     already filled risks "
              "queueing ISOs you never wanted.")
    print()
    try:
        choice = input("Select [1]: ").strip().lower()
    except EOFError:
        choice = ""

    if choice == "q":
        _quit()
    if choice in ("", "1"):
        return preset_items("standard")
    if choice == "2":
        return preset_items("advanced")
    if choice == "3":
        return preset_items("everything")
    if choice == "4":
        return custom_select(dest=dest, manifest=manifest)
    if choice == "5":
        return custom_select(preselect={it[0] for it in preset_items("standard")},
                             dest=dest, manifest=manifest)
    if choice == "6" and installed:
        # Start from what the stick already holds -- the usual "just bring my
        # stick up to date" case, still adjustable before it runs.
        return custom_select(preselect=set(installed),
                             dest=dest, manifest=manifest)
    print("Unrecognized choice -> Standard")
    return preset_items("standard")


# (key, headline, what it does, upside, downside). Menu order, so the
# recommended method is first and a bare Enter selects it.
WINDOWS_METHODS = [
    ("massgrave", "massgrave  (recommended)",
     "Opens the massgrave.dev page, tells you which entry to click, then "
     "watches your Downloads folder and moves the ISO onto the stick.",
     "always works, no admin rights, nothing upstream can block it",
     "you click the download yourself, so the run is not unattended"),
    ("uup", "uup",
     "Pulls the build straight from Windows Update and assembles the ISO "
     "locally with DISM.",
     "official and unattended, no bot protection in the way",
     "Windows only, needs admin, ~40 minutes and ~15 GB of free space"),
    ("fido", "fido",
     "Resolves an official, direct Microsoft download link and fetches it.",
     "hands-off and light: no admin, no local build, no browser",
     "Windows only, and Microsoft's anti-bot 'Sentinel' blocks it "
     "unpredictably"),
]


def choose_windows_method():
    """Ask how the Windows ISOs should be fetched. Returns a method key.

    None of the three is simply better than the others, so the trade-off is
    spelled out rather than decided silently. The default is the one that
    cannot fail for reasons outside the user's control -- at the price of one
    manual click.
    """
    print("\nHow should the Windows ISOs be fetched?\n")
    for i, (key, headline, what, good, bad) in enumerate(WINDOWS_METHODS, 1):
        unavailable = key in ("uup", "fido") and os.name != "nt"
        print("  [%d] %s%s"
              % (i, headline, "   (unavailable here: needs Windows)"
                 if unavailable else ""))
        for line in textwrap.wrap(what, 68):
            print("      %s" % line)
        for sign, text in (("+", good), ("-", bad)):
            for n, line in enumerate(textwrap.wrap(text, 66)):
                print("      %s  %s" % (sign if n == 0 else " ", line))
        print()
    try:
        raw = input("Method [1]: ").strip()
    except EOFError:
        raw = ""
    if raw.isdigit() and 1 <= int(raw) <= len(WINDOWS_METHODS):
        return WINDOWS_METHODS[int(raw) - 1][0]
    return WINDOWS_METHODS[0][0]


def choose_windows(already, args):
    """Opt-in prompt for the Windows ISOs, plus how to fetch them.

    They are in no preset because none of the methods runs unattended -- see
    WINDOWS_METHODS. An explicit --windows on the command line wins; the method
    prompt only appears when the flag was left off.
    """
    wins = [it for it in CATALOG if it[4] == "windows"]
    if any(it[0] in already for it in wins):
        return []                       # already picked in the custom selector

    print("\nWindows ISOs?  (none of the ways to get them is fully unattended;")
    print("you pick which one to use in the next step)")
    try:
        ans = input("Add Windows ISOs? [y/N]: ").strip().lower()
    except EOFError:
        ans = ""
    if ans not in ("y", "yes", "j", "ja"):
        return []

    print()
    for i, it in enumerate(wins, 1):
        print("  [%d] %-24s %s" % (i, it[1], DESCRIPTIONS.get(it[0], "")))
    try:
        raw = input("Which? (e.g. 1,3 -- empty = 1,2,3): ").strip()
    except EOFError:
        raw = ""
    idx = parse_ranges(raw, len(wins)) if raw else [1, 2, 3]
    picked = [wins[i - 1] for i in idx]

    if picked and args.windows is None:
        args.windows = choose_windows_method()
    return picked


# --------------------------------------------------------------------------- #
# Windows (massgrave) build detection
# --------------------------------------------------------------------------- #
def detect_windows_builds():
    """Latest build number per edition, keyed by the catalog `win` value
    ('11', '10', '2022', '2025'), read from the massgrave markdown. Any lookup
    that fails keeps its fallback -- the build only shapes the target filename."""
    builds = {"11": "26200.8655", "10": "19045.6456",
              "2022": "20348.5256", "2025": "26100.32995"}   # fallbacks
    try:
        md = http_get("https://raw.githubusercontent.com/massgravel/"
                      "massgrave.dev/main/docs/windows_11_links.md")
        m = re.search(r"Consumer 25H2.{0,60}?Build - ([\d.]+)[^\n]*Latest",
                      md, re.S)
        if m:
            builds["11"] = m.group(1)
    except Exception:
        pass
    try:
        md = http_get("https://raw.githubusercontent.com/massgravel/"
                      "massgrave.dev/main/docs/windows_10_links.md")
        m = re.search(r"Build - (19045\.\d+)", md)
        if m:
            builds["10"] = m.group(1)
    except Exception:
        pass
    try:
        md = http_get("https://raw.githubusercontent.com/massgravel/"
                      "massgrave.dev/main/docs/windows-server-links.md")
        for year in ("2025", "2022"):
            m = re.search(r"Windows Server %s.*?Build - ([\d.]+)" % year,
                          md, re.S)
            if m:
                builds[year] = m.group(1)
    except Exception:
        pass
    return builds


# --------------------------------------------------------------------------- #
# Windows via Fido (official Microsoft ISOs), with massgrave fallback
# --------------------------------------------------------------------------- #
def _powershell():
    return shutil.which("powershell") or shutil.which("pwsh")


def _is_admin():
    try:
        import ctypes
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except Exception:
        return False


def _relaunch_as_admin():
    """Relaunch this script elevated via UAC. Returns True if a child was
    launched (the caller should then exit); False if elevation was refused."""
    try:
        import ctypes
        script = os.path.abspath(sys.argv[0])
        params = " ".join('"%s"' % a for a in ([script] + sys.argv[1:]))
        rc = ctypes.windll.shell32.ShellExecuteW(
            None, "runas", sys.executable, params, None, 1)
        return int(rc) > 32
    except Exception:
        return False


_FIDO_PATH = None


def _ensure_fido():
    """Download the newest Fido.ps1 once per run into the temp dir."""
    global _FIDO_PATH
    if _FIDO_PATH and os.path.exists(_FIDO_PATH):
        return _FIDO_PATH
    path = os.path.join(tempfile.gettempdir(), "ventoy_fido.ps1")
    req = urllib.request.Request(FIDO_URL, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=45, context=CTX) as r, \
            open(path, "wb") as f:
        shutil.copyfileobj(r, f)
    _FIDO_PATH = path
    return path


def fido_url(ps, fido, win, lang, timeout=180):
    """Ask Fido for the official Microsoft direct-download URL (or raise)."""
    proc = subprocess.run(
        [ps, "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", fido,
         "-Win", win, "-Rel", "Latest", "-Lang", lang, "-Arch", "x64",
         "-GetUrl"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    out = (proc.stdout or b"").decode("utf-8", "replace")
    err = (proc.stderr or b"").decode("utf-8", "replace")
    url = ""
    for line in out.splitlines():
        line = line.strip()
        if line.lower().startswith("http"):
            url = line
    if not url:
        msg = next((l.strip() for l in out.splitlines()
                    if "error" in l.lower()), "") or err.strip() or "no URL"
        raise RuntimeError(msg[:140])
    return url


def windows_needs_build(dest, manifest, args, win_items):
    """True if at least one selected Windows ISO is missing/outdated."""
    for key, _l, _c, _t, _k, p in win_items:
        entry = manifest.get(key)
        stored = entry.get("filename") if entry else None
        if not (not args.force and stored and p["rel"] in stored
                and os.path.exists(os.path.join(dest, stored))):
            return True
    return False


def process_windows(dest, manifest, args, win_items, summary):
    """Download the selected Windows ISOs via the chosen method (--windows):
    massgrave hand-off (default), fido, or uup with a local build."""
    todo = []
    for key, label, _c, _t, _k, p in win_items:
        entry = manifest.get(key)
        stored = entry.get("filename") if entry else None
        if (not args.force and stored and p["rel"] in stored
                and os.path.exists(os.path.join(dest, stored))):
            print("==> %-24s %s  [up to date]" % (label, stored))
            summary.append((stored, "SKIP (up to date)"))
            print()
            continue
        todo.append((key, label, p))
    if not todo:
        return

    if args.windows == "massgrave":
        _massgrave_handoff(dest, manifest, todo, summary)
        return

    # Fido and UUP dump only cover consumer editions -- Server ISOs are
    # browser-only, so they stay on the massgrave hand-off regardless.
    server = [t for t in todo if t[2].get("server")]
    rest = [t for t in todo if not t[2].get("server")]
    if rest:
        if args.windows == "fido":
            _process_fido(dest, manifest, args, rest, summary)
        else:
            _process_uup(dest, manifest, args, rest, summary)
    if server:
        _massgrave_handoff(dest, manifest, server, summary)


# ---- UUP dump: official ISOs from Windows Update, built locally ---------- #
def _pid_alive(pid):
    """True if `pid` still refers to a running process (best-effort)."""
    try:
        pid = int(pid)
    except (TypeError, ValueError):
        return False
    if os.name != "nt":
        try:
            os.kill(pid, 0)
            return True
        except OSError:
            return False
    try:
        out = subprocess.run(["tasklist", "/FI", "PID eq %d" % pid, "/NH"],
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                             timeout=20).stdout or b""
        return str(pid).encode() in out
    except Exception:
        return False


def uup_latest_uuid(win, rel):
    """Newest *retail* amd64 build UUID + build number for Windows `win`/`rel`
    (e.g. '11'/'25H2') from the UUP dump index."""
    q = quote("Windows %s %s amd64" % (win, rel))
    data = json.loads(http_get("%s/listid.php?search=%s" % (UUP_API, q)))
    builds = data.get("response", {}).get("builds", {})
    items = builds if isinstance(builds, list) else list(builds.values())
    bad = ("insider", "dev", "beta", "canary", "preview")
    cand = [b for b in items
            if b.get("arch") == "amd64"
            and rel.lower() in (b.get("title") or "").lower()
            and not any(w in (b.get("title") or "").lower() for w in bad)]
    if not cand:
        raise RuntimeError("no retail UUP build for Windows %s %s" % (win, rel))
    best = max(cand, key=lambda b: verkey(b.get("build", "0")))
    return best["uuid"], best.get("build", "?")


def uup_build_iso(win, rel, lang, editions, work_root, dest, name_tmpl):
    """Fetch the UUP dump convert package, run it (downloads the UUP set from
    Windows Update and assembles the ISO with DISM), then move the finished
    ISO onto the stick. Returns (out_path, save_name, build). Needs admin."""
    uuid, build = uup_latest_uuid(win, rel)
    save_name = name_tmpl % build
    print("    build %s  (uuid %s)" % (build, uuid[:8]))

    url = "%s?%s" % (UUP_GET, urlencode(
        {"id": uuid, "pack": lang, "edition": editions}))
    body = urlencode({"autodl": "2", "updates": "1", "cleanup": "1"}).encode()
    req = urllib.request.Request(url, data=body, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=120, context=CTX) as r:
        pkg = r.read()

    # Build dir must be space-free (the UUP converter refuses spaces in paths).
    bdir = os.path.join(work_root, "uup_%s_%s_%s" % (win, lang, build))
    lock = os.path.join(bdir, ".building.pid")
    if os.path.exists(lock):
        # Another run (or a crashed one) owns this directory. Never clobber a
        # build in progress -- concurrent runs corrupt each other's downloads.
        try:
            with open(lock) as f:
                owner = f.read().strip()
        except OSError:
            owner = "?"
        if _pid_alive(owner):
            raise RuntimeError("another build (pid %s) is already using %s"
                               % (owner, bdir))
        print("    (clearing stale build dir from pid %s)" % owner)
    # Reset the scripts/converter but keep an already-downloaded UUP set: it is
    # several GB and aria2 skips files it already has, making retries cheap.
    if os.path.isdir(bdir):
        kept = 0
        for entry in os.listdir(bdir):
            p = os.path.join(bdir, entry)
            if entry == "UUPs":
                kept = len(os.listdir(p)) if os.path.isdir(p) else 0
                continue
            try:
                if os.path.isdir(p):
                    shutil.rmtree(p, ignore_errors=True)
                else:
                    os.remove(p)
            except OSError:
                pass
        if kept:
            print("    reusing %d already-downloaded UUP files" % kept)
    os.makedirs(bdir, exist_ok=True)
    with open(lock, "w") as f:
        f.write(str(os.getpid()))
    with zipfile.ZipFile(io.BytesIO(pkg)) as z:
        z.extractall(bdir)

    # The bundled get_aria2.ps1 downloads aria2c.exe and then verifies it with
    # Get-FileHash. If the freshly written binary is still locked (antivirus
    # scan) the check fails, it tries to re-download over the locked file and
    # the whole run aborts at a `pause`. Reuse a cached, known-good aria2c.exe
    # and stub the check out so the build never depends on that race.
    fdir = os.path.join(bdir, "files")
    os.makedirs(fdir, exist_ok=True)
    cache = os.path.join(work_root, "ventoy_aria2c.exe")
    if os.path.exists(cache):
        # Best-effort: if the target is locked (a concurrent build still has
        # aria2c.exe open) just leave the bundled downloader in place rather
        # than failing the whole ISO.
        try:
            shutil.copy(cache, os.path.join(fdir, "aria2c.exe"))
            with open(os.path.join(fdir, "get_aria2.ps1"), "w",
                      encoding="utf-8") as f:
                f.write("Exit 0\n")
        except OSError as e:
            print("    (could not pre-place aria2c.exe: %s -- using the "
                  "bundled downloader)" % e)

    # Make the converter exit cleanly instead of waiting on a keypress.
    cfg = os.path.join(bdir, "ConvertConfig.ini")
    with open(cfg, encoding="utf-8") as f:
        txt = f.read()
    txt = re.sub(r"AutoExit\s*=\s*0", "AutoExit     =1", txt)
    with open(cfg, "w", encoding="utf-8") as f:
        f.write(txt)

    print("    downloading the UUP set and building the ISO "
          "(this takes a while)...")
    cmd = os.path.join(bdir, "uup_download_windows.cmd")
    log = os.path.join(bdir, "build.log")
    # The UUP scripts invoke their helpers by bare name (`call convert-UUP.cmd`).
    # NoDefaultCurrentDirectoryInExePath -- often set by corporate policy --
    # stops cmd.exe resolving those from the working directory, so the
    # conversion silently never runs and no ISO appears. Drop it for the child.
    env = {k: v for k, v in os.environ.items()
           if k.lower() != "nodefaultcurrentdirectoryinexepath"}
    with open(log, "wb") as lf:
        subprocess.run(["cmd", "/c", cmd], cwd=bdir, env=env,
                       stdin=subprocess.DEVNULL, stdout=lf,
                       stderr=subprocess.STDOUT, timeout=7200)

    # Keep a good aria2c.exe for the next build (see the stub above).
    got_aria2 = os.path.join(fdir, "aria2c.exe")
    if not os.path.exists(cache) and os.path.exists(got_aria2):
        try:
            shutil.copy(got_aria2, cache)
        except Exception:
            pass

    isos = [f for f in os.listdir(bdir) if f.lower().endswith(".iso")]
    if not isos:
        raise RuntimeError("UUP produced no ISO (see %s)" % log)
    src = max((os.path.join(bdir, f) for f in isos), key=os.path.getsize)
    out = os.path.join(dest, save_name)
    if os.path.exists(out):
        os.remove(out)
    move_with_progress(src, out)          # scratch dir -> stick, several GB
    shutil.rmtree(bdir, ignore_errors=True)
    return out, save_name, build


def _process_uup(dest, manifest, args, todo, summary):
    if os.name == "nt" and not _is_admin():
        print("    UUP dump assembles the ISO with DISM and needs admin rights.")
        print("    Re-run this script as administrator to build Windows ISOs.\n")
        for _k, _l, p in todo:
            summary.append((p["tmpl"] % p["rel"], "SKIP (needs admin for UUP)"))
        return
    langmap = {"German": "de-de", "English": "en-us"}
    work = tempfile.gettempdir()
    for key, label, p in todo:
        lang = langmap.get(p["lang"], "en-us")
        print("==> %-24s UUP dump build (Win %s, %s)" % (label, p["win"], lang))
        try:
            out, name, build = uup_build_iso(
                p["win"], p["rel"], lang, "core;professional", work, dest,
                p["tmpl"])
            gb = os.path.getsize(out) / (1024 ** 3)
            print("    ok  %s  (%.2f GB)\n" % (name, gb))
            record_version(dest, manifest, key, "uup:%s/%s" % (p["win"], build),
                           name, (os.path.getsize(out), None))
            summary.append((name, "OK (built via UUP)"))
        except Exception as e:
            print("    UUP build failed: %s\n" % e)
            summary.append((p["tmpl"] % p["rel"], "FAIL: %s" % e))


def _process_fido(dest, manifest, args, todo, summary):
    ps = _powershell()
    fallback = []
    fido = None
    for key, label, p in todo:
        if not ps:
            fallback.append((key, label, p))
            continue
        if fido is None:
            try:
                fido = _ensure_fido()
            except Exception as e:
                print("    could not fetch Fido (%s) -> massgrave fallback" % e)
                fallback.append((key, label, p))
                continue
        print("==> %-24s querying Microsoft via Fido (%s)..."
              % (label, p["lang"]))
        try:
            url = fido_url(ps, fido, p["win"], p["lang"])
            name = os.path.basename(urlparse(url).path) or (p["tmpl"] % p["rel"])
            fetch_tracked(dest, manifest, key, label, url, name, [], args,
                          summary)
        except Exception as e:
            print("    Fido failed (%s) -> massgrave fallback\n" % e)
            fallback.append((key, label, p))
    if fallback:
        _massgrave_handoff(dest, manifest, fallback, summary)


def _iso_files(folders):
    out = []
    for d in folders:
        if not os.path.isdir(d):
            continue
        try:
            for f in os.listdir(d):
                if f.lower().endswith(".iso"):
                    out.append(os.path.join(d, f))
        except OSError:
            pass
    return out


def _match_score(fname, win, lang):
    """How well a downloaded filename matches an expected Windows ISO."""
    n = fname.lower().replace("-", "_").replace(" ", "_")
    # Consumer names read "win_11"/"windows_10"; Server names "windows_server_2025".
    if re.search(r"(win(dows)?|server)_?%s(?!\d)" % win, n):
        score = 2
    else:
        return 0                       # wrong Windows version
    words = {"German": ("german", "deutsch", "de_de"),
             "English": ("english", "en_us")}.get(lang, ())
    if any(w in n for w in words):
        score += 2
    return score


WATCH_POLL = 2.0          # seconds between folder scans
WATCH_STABLE = 3.0        # a file counts as finished once its size held this long
WATCH_HEARTBEAT = 60.0    # seconds between "still waiting" notes


def windows_watch_rename(dest, manifest, plan, summary, wait_min=90):
    """Watch the stick and the Downloads folder for the manually downloaded
    Windows ISOs, then move/rename each to its canonical name.

    A finished file is recognized by its size holding steady for WATCH_STABLE
    seconds -- measured as a timestamp, not as "unchanged between two polls".
    That distinction matters: moving an ISO onto the stick blocks this loop
    for minutes, and a poll-to-poll rule would make everything that finished
    downloading in the meantime wait out another full cycle afterwards."""
    downloads = os.path.join(os.path.expanduser("~"), "Downloads")
    folders = [dest, downloads]
    known = set(_iso_files(folders))          # ignore what's already there
    pending = list(plan)
    seen = {}                                 # path -> (size, when first seen)

    print("Watching for your downloads:")
    print("  %s" % dest)
    if os.path.isdir(downloads):
        print("  %s" % downloads)
    print("Each ISO is renamed and moved onto the stick automatically.")
    print("Copying one onto the stick takes a few minutes and pauses the")
    print("watch -- anything you download meanwhile is picked up right after.")
    print("Press Ctrl+C when you're done / want to skip the rest. DONT PANIC IF IT LOOKS STUCK!\n")

    started = time.monotonic()
    deadline = started + wait_min * 60
    next_beat = started + WATCH_HEARTBEAT
    try:
        while pending and time.monotonic() < deadline:
            for path in _iso_files(folders):
                if path in known:
                    continue
                try:
                    size = os.path.getsize(path)
                except OSError:
                    continue
                if size < 1024 ** 3:          # too small to be a Windows ISO
                    continue
                now = time.monotonic()
                prev = seen.get(path)
                if not prev or prev[0] != size:
                    seen[path] = (size, now)  # still growing -> restart the clock
                    continue
                if now - prev[1] < WATCH_STABLE:
                    continue                  # settled only just now
                base = os.path.basename(path)
                scored = sorted(((_match_score(base, e[3], e[4]), e)
                                 for e in pending),
                                key=lambda t: t[0], reverse=True)
                best_score, best = scored[0]
                if best_score < 4:
                    # Not every massgrave filename spells out the language.
                    # Take it anyway when exactly one pending ISO is left for
                    # that Windows version -- nothing it could be mixed up
                    # with. Otherwise keep waiting rather than guess wrong.
                    same = [e for s, e in scored if s >= 2]
                    if best_score < 2 or len(same) != 1:
                        continue
                    best = same[0]
                key, label, target = best[0], best[1], best[2]
                out = os.path.join(dest, target)
                try:
                    print("  %-28s -> %s" % (base, target))
                    if os.path.abspath(path) != os.path.abspath(out):
                        move_with_progress(path, out)
                    record_version(dest, manifest, key, "massgrave (manual)",
                                   target, (os.path.getsize(out), None))
                    summary.append((target, "OK (manual, renamed)"))
                    pending.remove(best)
                    known.add(path)
                    known.add(out)            # don't re-scan what we just wrote
                    seen.pop(path, None)
                    next_beat = time.monotonic() + WATCH_HEARTBEAT
                    if pending:
                        print("  watching again -- %d ISO(s) to go\n"
                              % len(pending))
                except (OSError, RuntimeError) as e:
                    print("  could not move %s: %s" % (base, e))
                    known.add(path)
            if not pending:
                break
            now = time.monotonic()
            if now >= next_beat:
                mins = int(now - started) // 60
                print("  still waiting for %d ISO(s) -- %d min elapsed"
                      % (len(pending), mins))
                next_beat = now + WATCH_HEARTBEAT
            time.sleep(WATCH_POLL)
    except KeyboardInterrupt:
        print("\n  (stopped watching)")

    for entry in pending:
        summary.append((entry[2], "MANUAL (still missing)"))


def _massgrave_handoff(dest, manifest, items, summary, watch=True):
    """Open the massgrave pages, then watch for and rename what gets saved."""
    builds = detect_windows_builds()
    plan = []      # (key, label, target_name, win, lang, page, pick)
    for key, label, p in items:
        build = builds.get(p["win"], "unknown")
        plan.append((key, label, p["tmpl"] % build, p["win"], p["lang"],
                     p["page"], p["pick"]))

    print("\n" + "=" * 16 + " WINDOWS ISOs (massgrave.dev) " + "=" * 16)
    print("massgrave's Windows ISOs are browser-only (rotating mirror links),")
    print("so these need a manual click. Opening the pages now -- pick:\n")
    for _k, _l, save, _w, _lg, _pg, pick in plan:
        print("  %-48s -> %s" % (pick, save))
    print("\nSave them anywhere (the stick or your Downloads folder);")
    print("the script picks them up and renames them for you.\n")

    for page in dict.fromkeys(pg for *_r, pg, _p in plan):   # unique, ordered
        try:
            webbrowser.open(page)
        except Exception:
            print("  (open manually: %s)" % page)

    if watch:
        windows_watch_rename(dest, manifest, plan, summary)
    else:
        for _k, _l, save, *_r in plan:
            summary.append((save, "MANUAL (massgrave browser download)"))


# --------------------------------------------------------------------------- #
# Ventoy auto-install templates
#
# Ventoy attaches an unattended-setup template to an image by exact path. The
# Windows ISOs carry their build number in the filename, so every update
# silently orphans the entry written for the previous build -- the ISO boots,
# and the answer file it was supposed to use is simply not applied.
#
# Templates are therefore assigned by the folder they sit in rather than named
# per image: everything in /template/win11 applies to every Windows 11 image on
# the stick, /template/win10 likewise. The image paths are rebuilt from the
# manifest on every run, so they follow the ISOs through updates and renames.
# --------------------------------------------------------------------------- #
TEMPLATE_ROOT = "template"
VENTOY_JSON = ("ventoy", "ventoy.json")


def _real_dirname(parent, wanted):
    """The on-disk spelling of `wanted` inside `parent`, or None.

    A stick formatted exFAT is case-insensitive, so a folder made as
    "Template/Win11" is found on Windows and missed on Linux -- the same stick
    would quietly lose its templates depending on which system updates it.
    Match without regard to case, then hand back the name as it is actually
    spelled, so the path written into ventoy.json is one that exists.
    """
    try:
        entries = os.listdir(parent)
    except OSError:
        return None
    if wanted in entries:
        return wanted
    lowered = wanted.lower()
    for name in entries:
        if name.lower() == lowered:
            return name
    return None


def template_dir(dest, folder):
    """(stick_path, local_path) of /template/<folder>, in its real spelling."""
    root = _real_dirname(dest, TEMPLATE_ROOT) or TEMPLATE_ROOT
    sub = _real_dirname(os.path.join(dest, root), folder) or folder
    return "/%s/%s" % (root, sub), os.path.join(dest, root, sub)


def stick_templates(dest, folder):
    """Stick-absolute paths of the .xml templates in /template/<folder>."""
    stick, local = template_dir(dest, folder)
    try:
        names = sorted(n for n in os.listdir(local)
                       if n.lower().endswith(".xml"))
    except OSError:
        return []
    return ["%s/%s" % (stick, n) for n in names]


def windows_on_stick(dest, manifest):
    """[(folder, image_name)] of the Windows images actually present."""
    found, seen = [], set()
    for key, _label, _cat, _tier, kind, payload in CATALOG:
        if kind != "windows":
            continue
        name = (manifest.get(key) or {}).get("filename")
        if not name or name in seen:
            continue
        if not os.path.exists(os.path.join(dest, name)):
            continue                      # recorded but not actually here
        seen.add(name)
        found.append(("win%s" % payload["win"], name))
    return found


def ensure_template_dirs(dest, manifest, create):
    """Make sure a template folder exists for each Windows image on the stick.

    Without this the feature is invisible: an empty stick has no /template, so
    there is nowhere obvious to put an answer file and nothing hinting that
    dropping one there would do anything. Returns the folders that exist but
    are still empty, so the caller can say so.
    """
    empty = []
    for folder in sorted({f for f, _name in windows_on_stick(dest, manifest)}):
        stick, local = template_dir(dest, folder)
        if create and not os.path.isdir(local):
            try:
                os.makedirs(local)
            except OSError:
                continue
        if os.path.isdir(local) and not stick_templates(dest, folder):
            empty.append(stick)
    return empty


def build_auto_install(dest, manifest):
    """The auto_install entries the Windows ISOs on the stick should have."""
    entries = []
    for folder, name in windows_on_stick(dest, manifest):
        templates = stick_templates(dest, folder)
        if templates:
            entries.append({"image": "/" + name, "template": templates})
    return entries


def _is_managed(entry):
    """Whether an auto_install entry came from here.

    Ownership is decided by the templates an entry points at, not by the image
    it names. An entry left from a previous Windows build names an ISO that no
    longer exists, and matching on current names would preserve precisely the
    stale entries this is meant to clear out.
    """
    templates = entry.get("template") if isinstance(entry, dict) else None
    if not isinstance(templates, list) or not templates:
        return False
    # Compared lowercased for the same reason the folders are looked up that
    # way: the stick is case-insensitive, so an entry written as /Template/Win11
    # is ours just as much as /template/win11.
    prefix = "/%s/win" % TEMPLATE_ROOT
    return all(isinstance(t, str) and t.lower().startswith(prefix)
               for t in templates)


def sync_auto_install(dest, manifest, args):
    """Rewrite ventoy.json's auto_install to match the templates on the stick.

    Only entries this script owns are touched; hand-written ones for other
    images are carried over untouched, and the rest of ventoy.json -- theme,
    menu settings, anything else -- is preserved as it was.
    """
    path = os.path.join(dest, *VENTOY_JSON)
    try:
        with open(path, "r", encoding="utf-8") as fh:
            config = json.load(fh)
    except (OSError, ValueError):
        config = {}
    if not isinstance(config, dict):
        print("\nventoy.json is not a JSON object -- leaving it alone.")
        return

    empty = ensure_template_dirs(dest, manifest, create=not args.dry_run)

    existing = config.get("auto_install")
    existing = existing if isinstance(existing, list) else []
    foreign = [e for e in existing if not _is_managed(e)]
    wanted = build_auto_install(dest, manifest)

    before = [e for e in existing if _is_managed(e)]
    if before == wanted:
        # Nothing to rewrite, but an empty folder is still worth one line --
        # it is the only sign that answer files can go there at all.
        if empty:
            print("\nNo unattended template in %s yet -- put an .xml there and"
                  "\nit gets attached to the matching Windows images."
                  % " or ".join(empty))
        return
    if not wanted and not foreign and "auto_install" not in config:
        return

    print("\n" + "-" * 12 + " AUTO-INSTALL TEMPLATES " + "-" * 12)
    for entry in wanted:
        was = next((e for e in before if e.get("image") == entry["image"]), None)
        mark = "unchanged" if was == entry else ("updated" if was else "added")
        print("  %-9s %s" % (mark, entry["image"]))
        for template in entry["template"]:
            print("            %s" % template)
    for entry in before:
        if not any(e.get("image") == entry.get("image") for e in wanted):
            print("  %-9s %s" % ("dropped", entry.get("image")))
    for folder in empty:
        print("  %-9s %s  (put an .xml here)" % ("empty", folder))

    if args.dry_run:
        print("\n  (--dry-run -- ventoy.json not written)")
        return

    if wanted or foreign:
        config["auto_install"] = foreign + wanted
    else:
        config.pop("auto_install", None)

    tmp = path + ".tmp"
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(config, fh, indent=4, ensure_ascii=False)
            fh.write("\n")
        os.replace(tmp, path)
        print("\n  ventoy.json updated.")
    except OSError as exc:
        print("\n  could NOT write ventoy.json: %s" % exc)


# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #
def _farewell():
    """Print the parting line once as the program exits. Italic only on a real
    terminal, so redirected/piped output stays clean."""
    msg = "See you Space Cowboy!"
    if sys.stdout.isatty():
        msg = "\033[3m%s\033[0m" % msg      # ANSI italic
    print("\n" + msg)


def main():
    ap = argparse.ArgumentParser(description="Update Ventoy ISOs to newest.")
    ap.add_argument("--dest", default=None,
                    help="target folder / Ventoy drive "
                         "(if omitted, you get an interactive picker)")
    ap.add_argument("--version", action="version",
                    version="update_ventoy_isos.py %s" % __version__)
    ap.add_argument("--force", action="store_true",
                    help="re-download every ISO, ignoring the version manifest")
    ap.add_argument("--dry-run", action="store_true",
                    help="only report what would be downloaded, deleted or "
                         "left alone; writes nothing")
    ap.add_argument("--no-verify", action="store_true",
                    help="skip the SHA-256 check against the checksum file "
                         "published next to the image")
    ap.add_argument("--no-mirror-test", action="store_true",
                    help="skip the fastest-mirror speed test; use the default "
                         "source directly")
    ap.add_argument("--preset", choices=["standard", "advanced", "everything",
                                          "custom"],
                    help="skip the interactive menu and use this selection")
    ap.add_argument("--cleanup", choices=["ask", "yes", "no"], default="ask",
                    help="what to do with superseded ISOs still on the stick: "
                         "ask (default, lists them and prompts), yes (delete "
                         "without asking) or no (only list them)")
    # No default: staying None is what tells the interactive path that the
    # user has not decided yet, so it can ask. Resolved below.
    ap.add_argument("--windows", choices=["massgrave", "uup", "fido"],
                    default=None,
                    help="Windows ISO method, skipping the prompt: massgrave "
                         "(default, browser hand-off + automatic rename), uup "
                         "(official, builds the ISO locally, needs admin), or "
                         "fido (official direct link, often bot-blocked)")
    args = ap.parse_args()
    # Registered here, after parse_args -- so --help/--version, which exit during
    # parsing, stay clean, while every real run gets the send-off on the way out.
    atexit.register(_farewell)

    target = args.dest if args.dest else choose_dest()
    # expanduser first: shells expand a bare ~ before we see it, but cmd.exe and
    # a quoted "~/isos" don't -- without this that leading ~ becomes a folder
    # literally named "~" under the current directory.
    dest = os.path.abspath(os.path.expanduser(target))
    if not args.dry_run:
        os.makedirs(dest, exist_ok=True)
    manifest = load_manifest(dest)

    selection = choose_selection(args.preset, dest, manifest)
    # Windows is never part of a preset -- ask for it separately. That prompt
    # also settles the method, unless --windows already did.
    if args.preset is None:
        selection = selection + choose_windows({it[0] for it in selection},
                                               args)
    if args.windows is None:
        args.windows = WINDOWS_METHODS[0][0]
    iso_items = [it for it in selection if it[4] == "iso"]
    win_items = [it for it in selection if it[4] == "windows"]
    mem_items = [it for it in selection if it[4] == "memtest"]
    if not selection:
        print("Nothing selected. Exiting.")
        return

    # UUP dump builds the Windows ISO locally with DISM, which needs admin.
    # If Windows work is pending and we're not elevated, request elevation once
    # (a single UAC prompt) so the whole run can proceed unattended.
    if (win_items and args.windows == "uup" and os.name == "nt"
            and not _is_admin()
            and windows_needs_build(dest, manifest, args, win_items)):
        print("\nWindows ISOs (UUP dump) need administrator rights (DISM).")
        print("Requesting elevation -- please confirm the UAC prompt...")
        if _relaunch_as_admin():
            print("Continuing in the elevated window.")
            return
        print("Elevation refused -> Windows ISOs will be skipped this run.\n")

    print("\nTarget: %s" % dest)
    print("Version manifest: %s" % os.path.join(dest, MANIFEST_NAME))
    print("Selected: %d ISOs%s%s\n"
          % (len(iso_items),
             " + MemTest86" if mem_items else "",
             " + %d Windows" % len(win_items) if win_items else ""))

    # ---- phase 1: detect newest versions -------------------------------- #
    print("Checking for latest versions...\n")
    resolved = []            # (key, label, url, filename, mirrors)
    summary = []             # (name, status)
    for key, label, _cat, _tier, _kind, resolver in iso_items:
        try:
            res = resolver()
            url, name = res[0], res[1]
            mirrors = res[2] if len(res) > 2 else []
            resolved.append((key, label, url, name, mirrors))
            extra = "  (+%d mirrors)" % len(mirrors) if mirrors else ""
            print("  %-24s -> %s%s" % (label, name, extra))
        except Exception as e:
            print("  %-24s -> DETECT FAILED (%s)" % (label, e))
            summary.append((label, "FAIL (detect): %s" % e))
    if mem_items:
        print("  %-24s -> memtest86-usb.img (latest zip)" % "MemTest86")
    if win_items:
        how = {"uup": "UUP dump (official, local build)",
               "fido": "Fido (official MS link)",
               "massgrave": "massgrave hand-off"}[args.windows]
        print("  %-24s -> %d ISO(s) via %s"
              % ("Windows", len(win_items), how))
    print()

    # ---- phase 2: sequential downloads (only what changed) -------------- #
    print("Starting sequential downloads...\n")
    for key, label, url, name, mirrors in resolved:
        fetch_tracked(dest, manifest, key, label, url, name, mirrors,
                      args, summary)

    # MemTest86 (special: zip -> img)
    if mem_items:
        mt_url = "https://www.memtest86.com/downloads/memtest86-usb.zip"
        mt_entry = manifest.get("memtest")
        if args.force:
            mt_need, mt_reason, mt_meta = True, "forced", None
        else:
            mt_need, mt_reason, mt_meta = check_update(
                dest, mt_entry, mt_url, "memtest86-usb.img")
        print("==> %-24s memtest86-usb.img  [%s]" % ("MemTest86", mt_reason))
        if args.dry_run:
            summary.append(("memtest86-usb.img",
                            "WOULD DOWNLOAD" if mt_need else "SKIP (up to date)"))
            print()
        elif not mt_need:
            summary.append(("memtest86-usb.img", "SKIP (up to date)"))
            print()
        else:
            try:
                download_memtest(dest)
                print("    ok\n")
                # record the source zip's size/mtime so reruns detect changes
                record_version(dest, manifest, "memtest", mt_url,
                               "memtest86-usb.img",
                               mt_meta or remote_meta(mt_url))
                summary.append(("memtest86-usb.img", "OK (updated)"))
            except Exception as e:
                print("    FAILED: %s\n" % e)
                summary.append(("memtest86-usb.img", "FAIL: %s" % e))

    # ---- Windows ISOs --------------------------------------------------- #
    # Every Windows method either drives a browser or builds an image locally,
    # so there is nothing a dry run could report without doing the work.
    if win_items and args.dry_run:
        print("==> %-24s %d item(s) skipped in --dry-run\n"
              % ("Windows", len(win_items)))
    elif win_items:
        process_windows(dest, manifest, args, win_items, summary)

    # ---- old versions left on the stick --------------------------------- #
    # Runs last so the manifest already holds this run's new filenames -- what
    # is left over can only be the previous versions. A dry run downgrades the
    # mode to "no", which still lists the leftovers but deletes nothing.
    extra = [("memtest", "MemTest86", "memtest86-usb.img")] if mem_items else []
    cleanup_superseded(dest, manifest,
                       current_files(manifest, resolved, extra),
                       "no" if args.dry_run else args.cleanup, summary)

    # After the cleanup, so the filenames the templates are wired to are the
    # ones that actually survived this run.
    sync_auto_install(dest, manifest, args)

    # ---- summary -------------------------------------------------------- #
    if not summary:
        print("Nothing to do.")
        return
    print("\n" + "=" * 16 + " SUMMARY " + "=" * 16)
    width = max(len(n) for n, _ in summary)
    for name, status in summary:
        print("  %-*s  %s" % (width, name, status))
    fails = sum(1 for _, s in summary if s.startswith("FAIL"))
    manual = sum(1 for _, s in summary if s.startswith("MANUAL"))
    updated = sum(1 for _, s in summary if s.startswith("OK"))
    skipped = sum(1 for _, s in summary if s.startswith("SKIP"))
    print("\nDone. %d items | %d updated | %d up-to-date | %d failed | "
          "%d manual (Windows)."
          % (len(summary), updated, skipped, fails, manual))
    if fails:
        print("For any FAILED item, download it manually into: %s" % dest)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nAborted.")
        sys.exit(1)
