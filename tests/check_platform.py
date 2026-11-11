#!/usr/bin/env python3
"""Platform-dependent checks: drive detection and the ventoy.json rewriting.

Everything here is offline, so it can run on every operating system the script
claims to support. That matters because those two areas are exactly where the
platforms differ -- one uses the Win32 API, /sys, or /Volumes depending on where
it runs, and the other has to survive a case-insensitive filesystem.

Exit code is 1 if any check failed, 0 otherwise.

    python tests/check_platform.py
"""

import importlib.util
import json
import os
import shutil
import sys
import tempfile
import time
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


def main():
    mod = load_module()
    print("=== platform: %s (%s) ===" % (sys.platform, os.name))
    results = [check_drive_detection(mod),
               check_auto_install(mod),
               check_template_dirs(mod),
               check_listing_cache(mod)]
    if all(results):
        return 0
    print("\nA platform-dependent check failed.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
