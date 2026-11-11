#!/usr/bin/env python3
"""Platform-dependent checks: drive detection and the ventoy.json rewriting.

Everything here is offline, so it can run on every operating system the script
claims to support. That matters because those two areas are exactly where the
platforms differ -- one uses the Win32 API, /sys, or /Volumes depending on where
it runs, and the other has to survive a case-insensitive filesystem.

The offline checks of the download path live here too -- the threaded detection
phase, the free-space check, unpacking MemTest86 -- so they run on all three.

Exit code is 1 if any check failed, 0 otherwise.

    python tests/check_platform.py
"""

import atexit
import collections
import importlib.util
import io
import json
import os
import shutil
import sys
import tempfile
import threading
import time
import zipfile
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
SOURCE = os.path.join(os.path.dirname(HERE), "update_ventoy_isos.py")


def load_module():
    spec = importlib.util.spec_from_file_location("update_ventoy_isos", SOURCE)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def check_drive_detection(mod):
    """detect_usb_drives() must answer on every platform, and answer sanely.

    Each platform takes a different route -- the Win32 drive type, the
    removable flag under /sys, the drvfs mounts under WSL, /Volumes on macOS --
    and a CI machine has no removable drive at all, so the expected result is
    usually an empty list. What is checked is that the call completes and that
    whatever it returns has the shape the picker relies on, because a wrong
    shape crashes the run right where the user picks a target.
    """
    try:
        drives = mod.detect_usb_drives()
    except Exception as exc:
        print("  FAIL drives         raised %s: %s" % (type(exc).__name__, exc))
        return False

    if not isinstance(drives, list):
        print("  FAIL drives         returned %s, expected a list"
              % type(drives).__name__)
        return False

    for entry in drives:
        if (not isinstance(entry, tuple) or len(entry) != 3
                or not isinstance(entry[0], str)
                or not isinstance(entry[1], str)
                or not isinstance(entry[2], int)):
            print("  FAIL drives         bad entry %r, want (path, label, size)"
                  % (entry,))
            return False
        if not os.path.isdir(entry[0]):
            print("  FAIL drives         %s is offered but is not a directory"
                  % entry[0])
            return False

    print("  ok   drives         %d removable drive(s): %s"
          % (len(drives), ", ".join(d[0] for d in drives) or "none"))
    return True


def _auto_install_round(mod, root_name, sub_name):
    """Run sync_auto_install once on a throwaway stick. -> (config, image)"""

    class Args(object):
        dry_run = False

    root = tempfile.mkdtemp(prefix="stick_")
    try:
        os.makedirs(os.path.join(root, "ventoy"))
        os.makedirs(os.path.join(root, root_name, sub_name))
        open(os.path.join(root, root_name, sub_name, "a.xml"), "w").close()
        current = "Win_11_25H2_English_99999.9999.iso"
        open(os.path.join(root, current), "w").close()
        open(os.path.join(root, "debian.iso"), "w").close()
        with open(os.path.join(root, "ventoy", "ventoy.json"), "w") as fh:
            json.dump({
                "theme": {"file": "/theme/x.txt"},
                "auto_install": [
                    {"image": "/Win_11_25H2_English_00000.0000.iso",
                     "template": ["/%s/%s/a.xml" % (root_name, sub_name)]},
                    {"image": "/debian.iso", "template": ["/mine/preseed.cfg"]},
                ]}, fh)

        mod.sync_auto_install(root, {"win11_en": {"filename": current}}, Args())

        with open(os.path.join(root, "ventoy", "ventoy.json")) as fh:
            return json.load(fh), current
    finally:
        shutil.rmtree(root, ignore_errors=True)


def check_auto_install(mod):
    """sync_auto_install rewrites the user's ventoy.json, so pin its edges.

    Three things lose data if they regress: an entry for a Windows build that
    has been replaced must be dropped rather than left pointing at a missing
    ISO, a hand-written entry for some other image must survive, and the rest
    of the file must come back untouched.

    Run twice, once with the folders spelled "Template/Win11". Ventoy sticks are
    exFAT and macOS formats APFS case-insensitively by default, while Linux is
    not -- so the same stick would silently lose its templates depending on
    which system last updated it, and the path written back has to be the one
    really on disk.
    """
    ok = True
    for root_name, sub_name in (("template", "win11"), ("Template", "Win11")):
        config, current = _auto_install_round(mod, root_name, sub_name)
        entries = config.get("auto_install", [])
        images = [e.get("image") for e in entries]
        templates = [t for e in entries for t in e.get("template", [])]

        problems = []
        if "/Win_11_25H2_English_00000.0000.iso" in images:
            problems.append("stale entry for a replaced build was kept")
        if "/" + current not in images:
            problems.append("no entry for the current image")
        if "/debian.iso" not in images:
            problems.append("hand-written entry was discarded")
        if "theme" not in config:
            problems.append("the rest of ventoy.json was lost")
        wanted = "/%s/%s/a.xml" % (root_name, sub_name)
        if wanted not in templates:
            problems.append("template path is not the on-disk spelling %s"
                            % wanted)

        label = "%s/%s" % (root_name, sub_name)
        if problems:
            for problem in problems:
                print("  FAIL auto-install   %-16s %s" % (label, problem))
            ok = False
        else:
            print("  ok   auto-install   %-16s stale dropped, current wired, "
                  "foreign kept" % label)
    return ok


def check_template_dirs(mod):
    """The template folders appear for the Windows images that are there.

    And only for those: a stick holding no Windows image must not sprout empty
    folders, and --dry-run must not create anything at all.
    """

    class Args(object):
        def __init__(self, dry_run=False):
            self.dry_run = dry_run

    manifest = {"win10_en": {"filename": "Win_10.iso"},
                "win11_en": {"filename": "Win_11.iso"}}

    def run(with_isos, dry_run):
        root = tempfile.mkdtemp(prefix="stick_")
        try:
            os.makedirs(os.path.join(root, "ventoy"))
            with open(os.path.join(root, "ventoy", "ventoy.json"), "w") as fh:
                json.dump({"theme": {"file": "/x"}}, fh)
            if with_isos:
                for record in manifest.values():
                    open(os.path.join(root, record["filename"]), "w").close()
            mod.sync_auto_install(root, manifest, Args(dry_run))
            base = os.path.join(root, "template")
            return sorted(os.listdir(base)) if os.path.isdir(base) else []
        finally:
            shutil.rmtree(root, ignore_errors=True)

    problems = []
    if run(True, False) != ["win10", "win11"]:
        problems.append("folders not created for the images present")
    if run(True, True):
        problems.append("--dry-run created folders")
    if run(False, False):
        problems.append("folders created without any Windows image")

    if problems:
        for problem in problems:
            print("  FAIL templates      %s" % problem)
        return False
    print("  ok   templates      created on demand, never under --dry-run")
    return True


def check_listing_cache(mod):
    """Resolvers sharing an index must fetch it once, threads or not.

    The four Proxmox entries read one directory, and that host times out when
    it gets them all at once. The resolvers run in a thread pool, where a plain
    "not cached yet?" check lets every one of them through.
    """
    calls = []

    def slow_get(url, timeout=45):
        calls.append(url)
        time.sleep(0.05)
        return "page"

    real, mod.http_get = mod.http_get, slow_get
    try:
        with ThreadPoolExecutor(8) as pool:
            pages = list(pool.map(mod.http_get_cached,
                                  ["https://example.invalid/iso/"] * 8))
    finally:
        mod.http_get = real
    if len(calls) == 1 and pages == ["page"] * 8:
        print("  ok   listing cache  8 threads, 1 fetch")
        return True
    print("  FAIL listing cache  8 threads fetched one index %d times"
          % len(calls))
    return False


def check_host_lock(mod):
    """No host gets more than one lookup at a time, however many threads run.

    Many entries share a host, and the sequential script never sent a host two
    requests at once. Keeping it that way is what lets the lookups run in
    threads without tripping rate limits -- or Proxmox, see check_listing_cache.

    After Ctrl+C the lookups still waiting for their turn must not go out at
    all, or the exit hangs until the whole queue has been sent.
    """
    state = {"active": 0, "peak": 0, "calls": 0}
    guard = threading.Lock()

    def fake_urlopen(req, **kw):
        with guard:
            state["active"] += 1
            state["calls"] += 1
            state["peak"] = max(state["peak"], state["active"])
        time.sleep(0.02)
        with guard:
            state["active"] -= 1
        return _FakeResponse(b"page")

    def lookup(i):
        url = "https://example.invalid/%d.iso" % i
        return mod.remote_meta(url) if i % 2 else mod.http_get(url)

    real, mod.urllib.request.urlopen = mod.urllib.request.urlopen, fake_urlopen
    try:
        with ThreadPoolExecutor(8) as pool:
            list(pool.map(lookup, range(8)))
        mod._ABORT.set()
        try:
            lookup(0)
        except RuntimeError:
            pass
    finally:
        mod._ABORT.clear()
        mod.urllib.request.urlopen = real
    if state["peak"] != 1:
        print("  FAIL host lock      one host saw %d requests at once"
              % state["peak"])
        return False
    if state["calls"] != 8:
        print("  FAIL host lock      a lookup was still sent after Ctrl+C")
        return False
    print("  ok   host lock      8 threads, 1 request at a time per host")
    return True


def check_idna_preloaded(mod):
    """The idna codec has to be loaded before the resolver threads start.

    Every name lookup encodes the host with it. Left to the threads, they race
    to load it on first use, and on Windows that failed up to 38 of 60 lookups
    with "unknown encoding: idna". The import in the script looks unused, so
    this is what keeps it from being tidied away.
    """
    if "encodings.idna" in sys.modules:
        print("  ok   idna           codec loaded with the script")
        return True
    print("  FAIL idna           encodings.idna is not imported by the script")
    return False


def check_detect_phase(mod):
    """main() resolves and update-checks every item once, in catalog order.

    Both happen in a thread pool, so pin what must survive that: the report
    keeps the catalog order, a failing resolver costs only its own item, and
    the remote check is made once per item -- not again before the download.
    """
    def broken():
        raise RuntimeError("layout changed")

    catalog = [
        ("a", "Item A", "Test", "standard", "iso",
         lambda: ("https://example.invalid/a-2.iso", "a-2.iso")),
        ("b", "Item B", "Test", "standard", "iso", broken),
        ("c", "Item C", "Test", "standard", "iso",
         lambda: ("https://example.invalid/c-1.iso", "c-1.iso")),
    ]
    asked = []

    def fake_meta(url, timeout=30):
        asked.append(url)
        return 5, None

    root = tempfile.mkdtemp(prefix="stick_")
    open(os.path.join(root, "c-1.iso"), "w").close()
    with open(os.path.join(root, mod.MANIFEST_NAME), "w") as fh:
        json.dump({"c": {"filename": "c-1.iso", "size": 5}}, fh)

    real = (mod.CATALOG, mod.remote_meta, sys.argv, sys.stdout)
    mod.CATALOG, mod.remote_meta = catalog, fake_meta
    sys.argv = ["x", "--dest", root, "--dry-run", "--preset", "standard",
                "--no-update-check"]
    sys.stdout = io.StringIO()
    try:
        mod.main()
        out = sys.stdout.getvalue()
    except Exception as exc:
        out = "%s: %s" % (type(exc).__name__, exc)
    finally:
        mod.CATALOG, mod.remote_meta, sys.argv, sys.stdout = real
        atexit.unregister(mod._farewell)
        shutil.rmtree(root, ignore_errors=True)

    wanted = ["-> a-2.iso", "-> DETECT FAILED (layout changed)", "-> c-1.iso",
              "a-2.iso  [not on stick]", "c-1.iso  [up to date]",
              "Item B   FAIL (detect)", "a-2.iso  WOULD DOWNLOAD",
              "c-1.iso  SKIP (up to date)"]
    at = [out.find(w) for w in wanted]
    if -1 in at or at != sorted(at):
        print("  FAIL detect         report is wrong or out of order:\n%s" % out)
        return False
    if asked != ["https://example.invalid/c-1.iso"]:
        print("  FAIL detect         remote checks made: %r, want one for c-1.iso"
              % asked)
        return False
    print("  ok   detect         catalog order kept, one remote check per item")
    return True


def check_memtest_extract(mod):
    """The MemTest86 image goes from the zip onto the stick in one piece.

    It is unpacked straight to its place, so a zip that turns out broken
    halfway must leave the image that was already there alone, and no .part.
    """
    root = tempfile.mkdtemp(prefix="stick_")
    image = os.path.join(root, "memtest86-usb.img")
    good = os.path.join(root, "good.zip")
    with zipfile.ZipFile(good, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("ReadMe.txt", "x")
        z.writestr("memtest86-usb.img", b"new image" * 4096)
    with open(good, "rb") as fh:
        data = fh.read()
    # Flip the stored CRC-32 of the image (central directory, last entry).
    at = data.rindex(b"PK\x01\x02") + 16
    bad = os.path.join(root, "bad.zip")
    with open(bad, "wb") as fh:
        fh.write(data[:at] + bytes([data[at] ^ 0xFF]) + data[at + 1:])

    real = mod.download
    problems = []
    try:
        mod.download = lambda url, path, sha256=None: shutil.copy(good, path)
        mod.download_memtest(root)
        with open(image, "rb") as fh:
            if fh.read() != b"new image" * 4096:
                problems.append("extracted image differs from the zip's")

        mod.download = lambda url, path, sha256=None: shutil.copy(bad, path)
        try:
            mod.download_memtest(root)
            problems.append("a zip with a wrong CRC was accepted")
        except zipfile.BadZipFile:
            pass
        with open(image, "rb") as fh:
            if fh.read() != b"new image" * 4096:
                problems.append("a broken zip replaced the good image")
        if os.path.exists(image + ".part"):
            problems.append(".part left on the stick")
    finally:
        mod.download = real
        shutil.rmtree(root, ignore_errors=True)

    for problem in problems:
        print("  FAIL memtest        %s" % problem)
    if not problems:
        print("  ok   memtest        unpacked in place, broken zip changes nothing")
    return not problems


class _FakeResponse(object):
    """Just enough of an HTTP response for download()."""

    def __init__(self, body, code=200, headers=None):
        self.body, self.code = io.BytesIO(body), code
        self.headers = dict(headers or {}, **{"Content-Type": "application/x-iso"})
        self.headers.setdefault("Content-Length", str(len(body)))

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def geturl(self):
        return "https://example.invalid/x.iso"

    def getcode(self):
        return self.code

    def read(self, n=-1):
        return self.body.read(n)


def check_free_space(mod):
    """download() refuses what cannot fit, before it writes anything.

    Running out of room mid-download leaves a .part that fills the stick. The
    check must count a .part being resumed as space already taken care of,
    or the last stretch of a nearly finished download would be refused.
    """
    root = tempfile.mkdtemp(prefix="stick_")
    dest = os.path.join(root, "x.iso")
    real = (mod.urllib.request.urlopen, mod.shutil.disk_usage)
    problems = []

    usage = collections.namedtuple("usage", "total used free")

    def free(n):
        mod.shutil.disk_usage = lambda path: usage(100, 100 - n, n)

    try:
        with open(dest, "wb") as fh:
            fh.write(b"old")
        mod.urllib.request.urlopen = \
            lambda req, **kw: _FakeResponse(b"0123456789")
        free(9)
        try:
            mod.download("https://example.invalid/x.iso", dest)
            problems.append("10 bytes were written into 9 free")
        except RuntimeError as exc:
            if "free space" not in str(exc):
                problems.append("wrong error: %s" % exc)
        if os.path.exists(dest + ".part"):
            problems.append("a .part was created although nothing fits")
        with open(dest, "rb") as fh:
            if fh.read() != b"old":
                problems.append("the existing file was touched")

        # 6 of 10 bytes are already in the .part: 4 to go, 5 free -> fits.
        with open(dest + ".part", "wb") as fh:
            fh.write(b"012345")
        mod.urllib.request.urlopen = lambda req, **kw: _FakeResponse(
            b"6789", 206, {"Content-Range": "bytes 6-9/10"})
        free(5)
        try:
            mod.download("https://example.invalid/x.iso", dest)
        except Exception as exc:
            problems.append("resume refused: %s" % exc)
        with open(dest, "rb") as fh:
            if fh.read() != b"0123456789":
                problems.append("resumed file is not complete")
    finally:
        mod.urllib.request.urlopen, mod.shutil.disk_usage = real
        shutil.rmtree(root, ignore_errors=True)

    for problem in problems:
        print("  FAIL free space     %s" % problem)
    if not problems:
        print("  ok   free space     too big refused up front, resume counted")
    return not problems


def main():
    mod = load_module()
    print("=== platform: %s (%s) ===" % (sys.platform, os.name))
    results = [check_drive_detection(mod),
               check_auto_install(mod),
               check_template_dirs(mod),
               check_listing_cache(mod),
               check_host_lock(mod),
               check_idna_preloaded(mod),
               check_detect_phase(mod),
               check_memtest_extract(mod),
               check_free_space(mod)]
    if all(results):
        return 0
    print("\nA platform-dependent check failed.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
