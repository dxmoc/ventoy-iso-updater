#!/usr/bin/env python3
"""Smoke-test every resolver in the catalog against the live upstreams.

The script depends on ~50 foreign directory layouts. When one of them changes,
the matching resolver stops finding an ISO -- and the only way to notice is to
ask the upstreams. Run this on a schedule so a broken layout shows up here
instead of in front of the Ventoy stick.

Exit code is 1 if any resolver failed, 0 otherwise.

    python tests/check_resolvers.py
"""

import concurrent.futures as cf
import importlib.util
import os
import re
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SOURCE = os.path.join(os.path.dirname(HERE), "update_ventoy_isos.py")


def load_module():
    spec = importlib.util.spec_from_file_location("update_ventoy_isos", SOURCE)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def resolve(entry):
    """Call one resolver, never raise. -> (key, label, ok, detail)"""
    key, label, fn = entry
    try:
        result = fn()
        return key, label, True, result[1]
    except Exception as exc:
        return key, label, False, "%s: %s" % (type(exc).__name__, exc)


def check_catalog(mod):
    """Every 'iso' catalog entry must resolve to a download.

    Failures are retried once, sequentially and after a pause. A mirror that
    times out under twelve parallel requests is a network hiccup, not a broken
    layout, and a check that cries wolf is a check nobody reads.
    """
    entries = [(key, label, payload)
               for key, label, _cat, _tier, kind, payload in mod.CATALOG
               if kind == "iso"]
    by_key = {key: entry for entry in entries for key in (entry[0],)}

    with cf.ThreadPoolExecutor(max_workers=12) as pool:
        results = list(pool.map(resolve, entries))

    flaky = [r[0] for r in results if not r[2]]
    if flaky:
        print("  retrying %d after a pause: %s\n" % (len(flaky), ", ".join(flaky)))
        time.sleep(5)
        retried = {key: resolve(by_key[key]) for key in flaky}
        results = [retried.get(r[0], r) for r in results]

    failed = [r for r in results if not r[2]]
    for key, label, ok, detail in sorted(results, key=lambda r: (r[2], r[0])):
        print("  %-4s %-14s %-24s %s"
              % ("ok" if ok else "FAIL", key, label, detail))
    print("\n%d of %d resolvers OK." % (len(results) - len(failed), len(results)))
    return not failed


def check_leap_is_current(mod):
    """openSUSE Leap must resolve to the newest release, not an older one.

    Leap 16 moved its installer to a new path and filename scheme. The old
    resolver did not match it, silently fell through to 15.6 and kept handing
    out an end-of-life release -- a passing resolver that returns the wrong
    answer. This pins the invariant that broke: whatever the newest release
    directory on the mirror is, the resolved filename must carry that version.
    """
    try:
        newest = mod.leap_versions()[0]
        _url, name = mod.r_opensuse_leap()
    except Exception as exc:
        print("  FAIL leap           %s: %s" % (type(exc).__name__, exc))
        return False

    found = re.search(r"(\d+\.\d+)", name)
    if found and found.group(1) == newest:
        print("  ok   leap           newest release %s -> %s" % (newest, name))
        return True
    print("  FAIL leap           newest release is %s but resolved %s -- the "
          "layout for %s probably changed" % (newest, name, newest))
    return False


def check_point_releases(mod):
    """A point release must sort above its base release.

    verkey used to flatten every integer in a filename into one tuple, so the
    "64" of amd64 outranked the ".1" of 26.04.1 and every Ubuntu resolver kept
    handing out the .0 image -- again a passing resolver with the wrong answer.
    """
    pairs = [("ubuntu-26.04-desktop-amd64.iso", "ubuntu-26.04.1-desktop-amd64.iso"),
             ("ubuntu-24.04.5-desktop-amd64.iso", "ubuntu-24.04.5.1-desktop-amd64.iso"),
             ("linuxmint-22-cinnamon-64bit.iso", "linuxmint-22.1-cinnamon-64bit.iso")]
    bad = [(old, new) for old, new in pairs if not mod.verkey(new) > mod.verkey(old)]
    for old, new in bad:
        print("  FAIL verkey         %s does not sort above %s" % (new, old))
    if not bad:
        print("  ok   verkey         point releases sort above their base")
    return not bad


def check_catalog_integrity(mod):
    """Every entry needs a unique key and a description line for the picker."""
    keys = [entry[0] for entry in mod.CATALOG]
    ok = True

    duplicates = sorted({k for k in keys if keys.count(k) > 1})
    if duplicates:
        print("  FAIL duplicate catalog keys: %s" % ", ".join(duplicates))
        ok = False

    undescribed = [k for k in keys if k not in mod.DESCRIPTIONS]
    if undescribed:
        print("  FAIL no DESCRIPTIONS entry: %s" % ", ".join(undescribed))
        ok = False

    orphaned = [k for k in mod.DESCRIPTIONS if k not in keys]
    if orphaned:
        print("  FAIL DESCRIPTIONS entry for unknown key: %s" % ", ".join(orphaned))
        ok = False

    if ok:
        print("  ok   %d catalog entries, all keyed and described." % len(keys))
    return ok


def main():
    mod = load_module()
    print("=== catalog integrity ===")
    integrity_ok = check_catalog_integrity(mod)
    print("\n=== catalog resolvers ===")
    catalog_ok = check_catalog(mod)
    print("\n=== version sanity ===")
    leap_ok = check_leap_is_current(mod)
    point_ok = check_point_releases(mod)
    if integrity_ok and catalog_ok and leap_ok and point_ok:
        return 0
    print("\nSomething upstream changed. Fix the resolver, then rerun.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
