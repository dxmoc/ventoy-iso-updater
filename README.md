# update_ventoy_isos

Keeps the ISOs on a [Ventoy](https://www.ventoy.net/) stick up to date. One
script, no dependencies beyond the Python standard library, runs on Windows,
Linux and macOS.

Windows and Linux are verified against a real stick. macOS runs in CI on every
change, but no one has yet pointed it at actual hardware -- the parts that can
be checked without a USB port pass there, and finding a real drive is the part
that cannot. Reports welcome.

It auto-detects the newest version of every ISO in its catalog (directory
listings, SourceForge RSS, the GitHub API, the NixOS channel bucket,
massgrave.dev), downloads only what actually changed, and cleans up the
versions it replaced.

## Usage

```sh
python update_ventoy_isos.py                 # interactive: pick drive, then ISOs
python update_ventoy_isos.py --dest E:\      # explicit target
python update_ventoy_isos.py --dry-run       # report only, write nothing
python update_ventoy_isos.py --force         # ignore the manifest, redownload
```

Without `--dest` the script lists the removable drives it found and lets you
choose one, so there is no drive letter to type. Detection uses the Win32 drive
type on Windows, the removable flag under `/sys` on Linux, and `/Volumes` on
macOS.

It also works under WSL, where the stick is a drvfs mount with no block device
behind it: there the script asks Windows which drive is removable. If that WSL
setup has `.exe` interop disabled -- common on Arch images booting systemd,
where `systemd-binfmt` drops the `WSLInterop` registration -- it falls back to
the drvfs mounts under `/mnt/`, leaving out the system drive, and labels them
as unverified. WSL only automounts drives that existed when it started, so a
stick plugged in later needs mounting first; the picker prints the exact
command when it sees one.

## What it does

**Only downloads what changed.** Every downloaded ISO is recorded in
`ventoy_versions.json` next to the ISOs. On the next run the newest version
online is compared against that record; rolling releases that keep the same
filename are checked via the remote size / `Last-Modified` header. ISOs that
were already on the stick are adopted instead of re-downloaded -- including the
case where a download finished but was never recorded, which otherwise cost a
full re-fetch of several GB.

**Verifies what it downloads.** Where the source publishes a checksum -- a
`SHA256SUMS` for the directory, or a per-image `.sha256` / `.DIGESTS` -- the
finished file is hashed and compared before it is put in place, so a corrupted
or tampered image never reaches the stick. Roughly half of the catalog
publishes one; the rest falls back to the size check, and each download says
which of the two it got. `--no-verify` turns it off.

**Shows what it would do.** `--dry-run` runs the whole detection pass, prints
what would be downloaded, skipped or deleted, and writes nothing at all -- no
ISOs, no manifest, not even the target folder.

**Shows you what is on the stick** before you pick anything -- item, exact
filename, size -- and marks those entries in the picker, so nothing gets
selected twice. If the stick already holds something, the menu offers
*"Update what's there"*: start from the current set and just refresh it.

**Removes superseded versions.** After the downloads it finds ISOs that a newer
release replaced -- including ones the manifest never knew about -- lists each
one next to the file that superseded it, and deletes them once you confirm
(`--cleanup ask|yes|no`).

**~60 items in the catalog**, grouped by family: desktop distros, the official
Ubuntu flavours, server/enterprise (including the Proxmox products and XCP-ng),
security, rescue tools, BSD. Pick a preset
(Standard / Advanced / Everything) or individual numbers and ranges like
`1,3,5-9`, `+12`, `-4`.

**Downloads with resume support** (HTTP Range), showing the serving host and
live speed in MB/s and Mbit/s. Where a machine-readable mirror list exists
(Arch, Fedora, and Ubuntu for the country in your OS region settings), a few
mirrors are briefly speed-tested against the primary and the fastest one is
used -- disable with `--no-mirror-test`. The SHA-256 always comes from the
primary; a mirror whose own checksum disagrees is dropped.

**Update notice:** at startup the script asks the GitHub API whether a newer
release of itself is out and prints a link if so. It downloads and changes
nothing; offline it stays silent. `--no-update-check` skips the request.

**Windows ISOs** are in no preset, because none of the three ways to get them
runs unattended. Pick them and the script asks which method to use, spelling out
what each one costs; pass `--windows` to skip that prompt. Windows 11, 10 and
**Windows Server 2022 / 2025** are all offered here. Server is browser-only on
massgrave, so those always take the `massgrave` hand-off even under `--windows
fido`/`uup` (which don't cover Server).

| Method | What it does | Upside | Downside |
| --- | --- | --- | --- |
| `massgrave` *(default)* | opens the massgrave.dev page, says which entry to click, then watches your Downloads folder and files the ISO onto the stick | always works, no admin | you click the download yourself |
| `uup` | pulls the build from Windows Update and assembles the ISO locally with DISM | official and unattended | Windows only, needs admin, ~40 min and ~15 GB free |
| `fido` | resolves an official direct Microsoft download link | light, no admin, no browser | Windows only, blocked unpredictably by Microsoft's anti-bot "Sentinel" |

**Keeps unattended-install templates attached.** Ventoy matches an answer file
to an image by exact path, and the Windows ISOs carry their build number in the
filename -- so every update silently orphans the `auto_install` entry written
for the previous build. The ISO still boots, the answer file is just never
applied, and nothing says so.

Templates are therefore assigned by folder instead of per image. The folders are
created for you as soon as a Windows image is on the stick, so there is nowhere
to guess and nothing to look up:

```
template/
├── win10/          -> applied to every Windows 10 image on the stick
│   └── unattend.xml
├── win11/          -> applied to every Windows 11 image
│   └── autounattend.xml
└── win2025/        -> Windows Server uses the same scheme (win2022 / win2025)
    └── autounattend.xml
```

After each run the `auto_install` section of `ventoy/ventoy.json` is rebuilt
from the manifest, so the entries follow the ISOs through renames, drop the
ones whose image is gone, and pick up any `.xml` added to those folders. Several
templates in a folder are all attached, and Ventoy then offers the choice at
boot. Entries pointing anywhere other than `template/win10` or `template/win11`
are treated as hand-written and left alone, as is the rest of the file.

## Options

| Flag | Effect |
| --- | --- |
| `--dest PATH` | target folder / Ventoy drive (omit for the interactive picker) |
| `--dry-run` | report what would change; write nothing |
| `--force` | re-download everything, ignoring the manifest |
| `--no-verify` | skip the SHA-256 check |
| `--version` | print the version and exit |
| `--preset standard\|advanced\|everything\|custom` | skip the menu |
| `--cleanup ask\|yes\|no` | what to do with superseded ISOs (default `ask`) |
| `--no-mirror-test` | skip the fastest-mirror speed test |
| `--no-update-check` | don't ask GitHub for a newer release at startup |
| `--windows massgrave\|fido\|uup` | how to obtain the Windows ISOs |

## Requirements

Python 3.6+. Nothing else -- standard library only.

## Development

Two checks, for the two ways this breaks.

The catalog resolves download URLs from ~50 foreign directory listings, so the
usual failure is an upstream reorganizing its mirror. `check_resolvers` asks
every upstream whether its resolver still finds an ISO:

```sh
python tests/check_resolvers.py
```

The other failure is a platform difference. Drive detection takes a different
route per system -- Win32 API, the removable flag under `/sys`, drvfs under
WSL, `/Volumes` on macOS -- and the `ventoy.json` rewriting has to survive a
case-insensitive filesystem. `check_platform` covers both, offline, and runs on
Linux, Windows and macOS in CI:

```sh
python tests/check_platform.py
```

It also pins one invariant that already bit once: openSUSE Leap must resolve to
the newest release on the mirror. Leap 16 moved its installer to a new path and
filename, the resolver stopped matching, and instead of failing it quietly kept
serving end-of-life 15.6 -- so "the resolver returned something" is not enough
on its own.

Resolver failures are retried once before the check gives up, because a mirror
that times out under parallel requests is a hiccup, not a broken layout. GitHub
Actions runs both checks every Monday, and note that GitHub disables scheduled
workflows after 60 days without repository activity.

## License

MIT -- see [LICENSE](LICENSE).
