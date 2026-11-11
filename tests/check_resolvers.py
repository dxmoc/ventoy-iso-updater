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


def check_fedora_staged_release(mod):
    """A Fedora release staged ahead of release day must not break the resolver.

    Fedora 45 appeared in the release list days early with every subdirectory
    answering 404, and all three Fedora resolvers failed until release day.
    The previous release is still current then. Offline: the listings are fake.
    """
    rel = mod.FEDORA_RELEASES
    pages = {rel: '<a href="44/"> <a href="45/"> <a href="test/">',
             rel + "44/KDE/x86_64/iso/":
                 '<a href="Fedora-KDE-Desktop-Live-44-1.7.x86_64.iso">'}

    def fake_get(url, timeout=45):
        if url in pages:
            return pages[url]
        raise mod.urllib.error.HTTPError(url, 404, "Not Found", {}, None)

    real = (mod.http_get_cached, mod._fedora_mirrors)
    mod.http_get_cached, mod._fedora_mirrors = fake_get, lambda url: []
    try:
        _url, name, _mirrors = mod.r_fedora_kde()
    except Exception as exc:
        name = "%s: %s" % (type(exc).__name__, exc)
    finally:
        mod.http_get_cached, mod._fedora_mirrors = real
    if name == "Fedora-KDE-Desktop-Live-44-1.7.x86_64.iso":
        print("  ok   fedora         staged 45 skipped -> %s" % name)
        return True
    print("  FAIL fedora         staged 45 should fall back to 44, got %s" % name)
    return False


def check_freebsd_prerelease(mod):
    """A FreeBSD release directory holding only BETA images must be skipped.

    15.2/ appeared with nothing but BETA1 in it and the resolver failed until
    release day, although 15.1 was still the current release. Offline: the
    listings are fake.
    """
    pages = {"": '<a href="15.1/"> <a href="15.2/">',
             "15.2/": '<a href="FreeBSD-15.2-BETA1-amd64-disc1.iso">',
             "15.1/": '<a href="FreeBSD-15.1-RELEASE-amd64-disc1.iso">'}

    def fake_get(url, timeout=45):
        return pages[url.split("ISO-IMAGES/")[1]]

    real = (mod.http_get, mod.http_get_cached)
    mod.http_get = mod.http_get_cached = fake_get
    try:
        name = mod.r_freebsd()[1]
    except Exception as exc:
        name = "%s: %s" % (type(exc).__name__, exc)
    finally:
        mod.http_get, mod.http_get_cached = real
    if name == "FreeBSD-15.1-RELEASE-amd64-disc1.iso":
        print("  ok   freebsd        BETA-only 15.2 skipped -> %s" % name)
        return True
    print("  FAIL freebsd        BETA-only 15.2 should fall back to 15.1, got %s"
          % name)
    return False


def check_sums_ignore_html(mod):
    """A hex string inside an HTML page is not a published checksum.

    SourceForge answers "<image>.sha256" with its file browser. That page
    carries 64-digit hex in icon URLs and file metadata, and whenever exactly
    one turned up it was taken for the image's SHA-256 -- GParted downloaded
    in full and was then discarded as corrupt. A real one-line sums file must
    still count, whatever filename it mentions. Offline: the pages are fake.
    """
    digest = "ab" * 32
    html = '<html><img src="//a.example/icon?%s?w=48">' % ("cd" * 32)
    pages = {"https://example.invalid/a.iso.sha256": html,
             "https://example.invalid/b.iso.sha256":
                 "%s  upstream-build-name.iso\n" % digest}

    def fake_get(url, timeout=45):
        if url in pages:
            return pages[url]
        raise mod.urllib.error.HTTPError(url, 404, "Not Found", {}, None)

    real, mod.http_get = mod.http_get, fake_get
    try:
        from_html = mod.remote_sha256("https://example.invalid/a.iso", "a.iso")
        from_sums = mod.remote_sha256("https://example.invalid/b.iso", "b.iso")
    finally:
        mod.http_get = real
    if from_html is None and from_sums == digest:
        print("  ok   sums           HTML page ignored, one-line sums file read")
        return True
    print("  FAIL sums           HTML page gave %s, one-line sums file gave %s"
          % (from_html, from_sums))
    return False


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
    fedora_ok = check_fedora_staged_release(mod)
    freebsd_ok = check_freebsd_prerelease(mod)
    sums_ok = check_sums_ignore_html(mod)
    if (integrity_ok and catalog_ok and leap_ok and point_ok and fedora_ok
            and freebsd_ok and sums_ok):
        return 0
    print("\nSomething upstream changed. Fix the resolver, then rerun.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
