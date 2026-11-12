//! Removable-drive detection: Win32, /sys, WSL, /Volumes
//! (Python: _win_volume_label .. detect_usb_drives).

/// (path, label, total bytes) of a target the picker offers.
pub type Drive = (String, String, u64);

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

#[cfg(windows)]
fn win_volume_label(root: &str) -> String {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let mut buf = [0u16; 261];
    let mut fsbuf = [0u16; 261];
    // SAFETY: NUL-terminated root, buffers sized as passed, optional outputs null.
    let ok = unsafe {
        GetVolumeInformationW(
            wide(root).as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fsbuf.as_mut_ptr(),
            fsbuf.len() as u32,
        )
    };
    if ok == 0 {
        return String::new();
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

#[cfg(windows)]
fn mount_total(path: &str) -> u64 {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut total = 0u64;
    // SAFETY: NUL-terminated path; the two unused outputs are optional.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide(path).as_ptr(),
            std::ptr::null_mut(),
            &mut total,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 { 0 } else { total }
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // field types differ between Linux and macOS
fn mount_total(path: &str) -> u64 {
    let Ok(c) = std::ffi::CString::new(path) else {
        return 0;
    };
    // SAFETY: statvfs only writes into the zeroed struct we own.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return 0;
    }
    (st.f_blocks as u64).saturating_mul(st.f_frsize as u64)
}

/// os.path.ismount: a different device than the parent, or the root itself.
#[cfg(unix)]
fn is_mount(path: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let p = std::path::Path::new(path);
    let (Ok(m), Ok(parent)) = (
        std::fs::symlink_metadata(p),
        std::fs::symlink_metadata(p.join("..")),
    ) else {
        return false;
    };
    !m.file_type().is_symlink() && (m.dev() != parent.dev() || m.ino() == parent.ino())
}

/// True inside WSL, where the Windows drives appear as drvfs under /mnt.
#[cfg(unix)]
fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::env::var_os("WSL_INTEROP").is_some()
        || std::fs::read_to_string("/proc/version")
            .is_ok_and(|v| v.to_lowercase().contains("microsoft"))
}

/// (device, mountpoint, fstype)
#[cfg(unix)]
type Mount = (String, String, String);

#[cfg(unix)]
fn proc_mounts() -> Vec<Mount> {
    std::fs::read("/proc/mounts")
        .map(|b| parse_mounts(&String::from_utf8_lossy(&b)))
        .unwrap_or_default()
}

/// Mountpoints in /proc/mounts are octal-escaped (a space is "\040"), so decode
/// them before the path is usable.
#[cfg(unix)]
fn parse_mounts(raw: &str) -> Vec<Mount> {
    let esc = regex::Regex::new(r"\\([0-7]{3})").unwrap();
    raw.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                return None;
            }
            let mnt = esc.replace_all(parts[1], |c: &regex::Captures| {
                char::from_u32(u32::from_str_radix(&c[1], 8).unwrap())
                    .unwrap()
                    .to_string()
            });
            Some((parts[0].to_string(), mnt.into_owned(), parts[2].to_string()))
        })
        .collect()
}

/// Ask /sys whether the disk behind a /dev node is removable. Partitions carry
/// no flag of their own, so trim characters until /sys/block knows the name:
/// sdb1 -> sdb, nvme0n1p2 -> nvme0n1, mmcblk0p1 -> mmcblk0.
#[cfg(unix)]
fn block_is_removable(device: &str, sys_block: &std::path::Path) -> bool {
    let mut name = device.rsplit('/').next().unwrap_or("").to_string();
    while !name.is_empty() && !sys_block.join(&name).is_dir() {
        name.pop();
    }
    !name.is_empty()
        && std::fs::read_to_string(sys_block.join(&name).join("removable"))
            .is_ok_and(|s| s.trim() == "1")
}

/// Run a command, capture stdout; None on spawn error, failure or timeout.
#[cfg(unix)]
fn output_within(cmd: &mut std::process::Command, secs: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(secs);
    // shortcut: stdout is read after exit, fine for a few lines, would block past the 64 KiB pipe buffer
    let status = loop {
        if let Some(s) = child.try_wait().ok()? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    status.success().then_some(out)
}

/// [(letter, label, size)] of removable Windows drives, asked from Windows.
/// Under WSL a stick is a drvfs mount with no block device in /sys, so
/// nothing on the Linux side tells it from the system drive.
#[cfg(unix)]
fn wsl_windows_removable() -> Vec<(String, String, u64)> {
    const CMD: &str = "Get-CimInstance Win32_LogicalDisk | \
        Where-Object { $_.DriveType -eq 2 } | \
        ForEach-Object { \"$($_.DeviceID)|$($_.VolumeName)|$($_.Size)\" }";
    output_within(
        std::process::Command::new("powershell.exe").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            CMD,
        ]),
        25,
    )
    .map(|out| parse_wsl_removable(&String::from_utf8_lossy(&out)))
    .unwrap_or_default()
}

#[cfg(unix)]
fn parse_wsl_removable(out: &str) -> Vec<(String, String, u64)> {
    out.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.trim().split('|').collect();
            let letter = parts[0].chars().next()?;
            if parts.len() != 3 || !letter.is_alphabetic() {
                return None;
            }
            let digits = !parts[2].is_empty() && parts[2].bytes().all(|b| b.is_ascii_digit());
            let label = if parts[1].is_empty() {
                "(no label)"
            } else {
                parts[1]
            };
            Some((
                letter.to_lowercase().to_string(),
                label.to_string(),
                if digits {
                    parts[2].parse().unwrap_or(0)
                } else {
                    0
                },
            ))
        })
        .collect()
}

/// Windows drives under /mnt/<letter>, used when interop is unavailable (e.g.
/// Arch images booting systemd, where systemd-binfmt drops WSL's binfmt entry
/// and every .exe fails). Labeled unverified: nothing here says which drive is
/// removable. /mnt/c is skipped on purpose: the picker treats the first entry
/// as the default, and the system drive must never be it.
#[cfg(unix)]
fn drvfs_candidates(mounts: &[Mount]) -> Vec<Drive> {
    mounts
        .iter()
        .filter(|(_, _, fs)| matches!(fs.as_str(), "drvfs" | "9p" | "virtiofs"))
        .filter_map(|(_, mnt, _)| {
            let l = mnt
                .strip_prefix("/mnt/")
                .filter(|l| l.len() == 1 && l.as_bytes()[0].is_ascii_lowercase() && *l != "c")?;
            Some((
                mnt.clone(),
                format!("{}: windows drive (unverified)", l.to_uppercase()),
                mount_total(mnt),
            ))
        })
        .collect()
}

/// [(letter, label)] of removable Windows drives WSL has not mounted. WSL only
/// automounts what existed when it started, so a stick plugged in later stays
/// invisible until it is mounted by hand.
pub fn wsl_unmounted_drives() -> Vec<(String, String)> {
    #[cfg(unix)]
    if is_wsl() {
        return wsl_windows_removable()
            .into_iter()
            .filter(|(letter, _, _)| !is_mount(&format!("/mnt/{letter}")))
            .map(|(letter, label, _)| (letter, label))
            .collect();
    }
    Vec::new()
}

/// Removable drives, best-effort.
#[cfg(windows)]
pub fn detect_usb_drives() -> Vec<Drive> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    // SAFETY: no arguments.
    let bitmask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for (i, letter) in ('A'..='Z').enumerate() {
        if (bitmask >> i) & 1 == 0 {
            continue;
        }
        let root = format!("{letter}:\\");
        // SAFETY: NUL-terminated root path.
        if unsafe { GetDriveTypeW(wide(&root).as_ptr()) } != 2 {
            continue; // DRIVE_REMOVABLE
        }
        let label = win_volume_label(&root);
        let label = if label.is_empty() {
            "(no label)".to_string()
        } else {
            label
        };
        let total = mount_total(&root);
        drives.push((root, label, total));
    }
    drives
}

/// Removable drives, best-effort.
#[cfg(unix)]
pub fn detect_usb_drives() -> Vec<Drive> {
    use std::collections::HashSet;
    use std::path::Path;
    let mut drives = Vec::new();
    let mut seen = HashSet::new();

    // Linux: /proc/mounts plus the removable flag in /sys is authoritative and
    // does not care where (or whether) the desktop mounted the stick.
    for (device, mnt, _) in proc_mounts() {
        if !device.starts_with("/dev/")
            || seen.contains(&mnt)
            || !block_is_removable(&device, Path::new("/sys/block"))
        {
            continue;
        }
        let label = mnt
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&mnt)
            .to_string();
        let total = mount_total(&mnt);
        seen.insert(mnt.clone());
        drives.push((mnt, label, total));
    }

    // WSL: drvfs mounts have no block device, so they never show up above.
    // Ask Windows which of them is removable.
    if is_wsl() {
        let exact: Vec<Drive> = wsl_windows_removable()
            .into_iter()
            .map(|(letter, label, size)| (format!("/mnt/{letter}"), label, size))
            .collect();
        let found = if exact.is_empty() {
            drvfs_candidates(&proc_mounts())
        } else {
            exact
        };
        for (path, label, size) in found {
            if seen.contains(&path) || !is_mount(&path) {
                continue;
            }
            seen.insert(path.clone());
            let size = if size == 0 { mount_total(&path) } else { size };
            drives.push((path, label, size));
        }
    }

    // macOS has no /sys and mounts removable volumes under /Volumes; keep the
    // directory scan for it and as a fallback for anything missed above.
    let user = [std::env::var("USER"), std::env::var("LOGNAME")]
        .into_iter()
        .flatten()
        .find(|u| !u.is_empty())
        .unwrap_or_default();
    for base in [
        "/Volumes".to_string(),
        format!("/media/{user}"),
        "/media".to_string(),
        format!("/run/media/{user}"),
    ] {
        let Ok(dir) = std::fs::read_dir(&base) else {
            continue;
        };
        let mut entries: Vec<String> = dir
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        for entry in entries {
            let path = Path::new(&base).join(&entry).to_string_lossy().into_owned();
            if seen.contains(&path) || !is_mount(&path) {
                continue;
            }
            seen.insert(path.clone());
            let total = mount_total(&path);
            drives.push((path, entry, total));
        }
    }
    drives
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of check_platform.check_drive_detection: completes, and every
    /// offered path is a directory.
    #[test]
    fn detect_usb_drives_answers_sanely() {
        for (path, _, _) in detect_usb_drives() {
            assert!(
                std::path::Path::new(&path).is_dir(),
                "{path} is offered but is not a directory"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn mounts_are_unescaped() {
        let raw = "/dev/sdb1 /media/me/MY\\040STICK vfat rw 0 0\n\
                   C:\\134 /mnt/c 9p rw 0 0\nshort line\n";
        assert_eq!(
            parse_mounts(raw),
            vec![
                (
                    "/dev/sdb1".into(),
                    "/media/me/MY STICK".into(),
                    "vfat".into()
                ),
                ("C:\\134".into(), "/mnt/c".into(), "9p".into()),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn removable_flag_walks_up_to_the_disk() {
        let sys = std::env::temp_dir().join(format!("vui-sys-{}", std::process::id()));
        for (disk, flag) in [("sdb", "1"), ("nvme0n1", "0"), ("mmcblk0", "1\n")] {
            std::fs::create_dir_all(sys.join(disk)).unwrap();
            std::fs::write(sys.join(disk).join("removable"), flag).unwrap();
        }
        std::fs::create_dir_all(sys.join("sdc")).unwrap(); // no removable file
        assert!(block_is_removable("/dev/sdb1", &sys));
        assert!(block_is_removable("/dev/sdb", &sys));
        assert!(!block_is_removable("/dev/nvme0n1p2", &sys));
        assert!(block_is_removable("/dev/mmcblk0p1", &sys));
        assert!(!block_is_removable("/dev/sdc1", &sys));
        assert!(!block_is_removable("/dev/zzz", &sys));
        assert!(!block_is_removable("/dev/", &sys));
        std::fs::remove_dir_all(&sys).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn powershell_answer_is_parsed() {
        let out = "E:|STICK|31024349184\r\nF:||\r\n\r\n1:|x|5\nG:|a|b|c\nH:|X|n/a\n";
        assert_eq!(
            parse_wsl_removable(out),
            vec![
                ("e".into(), "STICK".into(), 31024349184),
                ("f".into(), "(no label)".into(), 0),
                ("h".into(), "X".into(), 0),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn drvfs_fallback_skips_system_drive() {
        let m = |mnt: &str, fs: &str| ("x".to_string(), mnt.to_string(), fs.to_string());
        let found = drvfs_candidates(&[
            m("/mnt/c", "9p"),
            m("/mnt/d", "drvfs"),
            m("/mnt/e", "virtiofs"),
            m("/mnt/f", "ext4"),
            m("/mnt/wsl", "9p"),
            m("/mnt/G", "drvfs"),
        ]);
        let got: Vec<_> = found
            .iter()
            .map(|(p, l, _)| (p.as_str(), l.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("/mnt/d", "D: windows drive (unverified)"),
                ("/mnt/e", "E: windows drive (unverified)")
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn root_is_a_mount_and_a_file_is_not() {
        assert!(is_mount("/"));
        assert!(!is_mount(file!()));
    }
}
