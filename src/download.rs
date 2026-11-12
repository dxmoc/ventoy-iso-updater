//! Downloader, progress line, moves, MemTest86, fetch_tracked
//! (Python: _progress .. download_memtest, fetch_tracked).

use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Result, bail};

use crate::Summary;
use crate::app::Args;
use crate::manifest::{self, Check, Manifest};
use crate::net::{self, Stream, fmt_size};
use crate::style::{dim, green, red};

/// Render one progress line: %, sizes, and live speed in MB/s + Mbit/s.
fn progress(done: u64, total: Option<u64>, bps: f64, last: bool) {
    let (mbps, mbit) = (bps / 1e6, bps * 8.0 / 1e6);
    let line = match total {
        Some(t) if t > 0 => format!(
            "    {:5.1}%  {} / {}  @ {mbps:6.2} MB/s ({mbit:6.1} Mbit/s)",
            done as f64 * 100.0 / t as f64,
            fmt_size(done),
            fmt_size(t)
        ),
        _ => format!(
            "    {}  @ {mbps:6.2} MB/s ({mbit:6.1} Mbit/s)",
            fmt_size(done)
        ),
    };
    // Piped, every redraw would be a line of its own: print one per 10 %
    // (or per 10 s when the size is unknown) instead.
    if !io::stdout().is_terminal() {
        static SHOWN: Mutex<Option<(u64, Instant)>> = Mutex::new(None);
        let tenth = total.filter(|&t| t > 0).map_or(0, |t| done * 10 / t);
        let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
        let (prev, at) = *shown.get_or_insert((0, Instant::now()));
        if last {
            *shown = None;
        } else if tenth > prev || at.elapsed().as_secs() >= 10 {
            *shown = Some((tenth, Instant::now()));
        } else {
            return;
        }
        println!("{line}");
        return;
    }
    // Pad to clear any leftover characters from a longer previous line.
    print!("\r{line}      {}", if last { "\n" } else { "" });
    let _ = io::stdout().flush();
}

/// A filename from upstream (listing, API, URL) must stay one plain entry in
/// the target folder: "a/../../x.iso" or "C:x.iso" would write elsewhere.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']) {
        bail!("unsafe filename from upstream: {name:?}");
    }
    Ok(())
}

/// Temp files a Ctrl+C must not leave behind; app's handler removes them.
static TEMPS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Registers a temp file or dir for removal on Ctrl+C until dropped.
pub struct Temp(PathBuf);

impl Temp {
    pub fn new(path: &Path) -> Self {
        TEMPS.lock().unwrap().push(path.to_path_buf());
        Temp(path.to_path_buf())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        TEMPS.lock().unwrap().retain(|p| *p != self.0);
    }
}

/// Ctrl+C: delete every registered temp path.
pub fn remove_temps() {
    for p in TEMPS.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        let _ = fs::remove_file(&p).or_else(|_| fs::remove_dir_all(&p));
    }
}

fn part_of(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".part");
    p.into()
}

/// Bytes free for the caller on the volume holding `dir`; None if unknown.
pub(crate) fn free_space(dir: &Path) -> Option<u64> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: c is a valid NUL-terminated path, st a writable statvfs.
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        #[allow(clippy::unnecessary_cast)] // field types differ per platform
        Some(st.f_bavail as u64 * st.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: wide is NUL-terminated; the two unused out-params may be null.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { None } else { Some(free) }
    }
}

/// url -> dest via dest.part with resume, live speed, optional SHA-256 check.
///
/// Only renamed onto `dest` once verified, so an existing good file is never
/// clobbered by a truncated or wrong-content (HTML error page) download. A
/// .part resumes via HTTP Range; a 200 (range ignored) restarts from scratch,
/// a 416 means the .part is already complete -- `net::open_stream` must hand
/// that back as `Ok` with `status == 416`. A SHA-256 mismatch deletes the
/// .part: resuming it would only rebuild the same bad file. A file that will
/// not fit is refused before the first byte is written.
pub fn download(url: &str, dest: &Path, sha256: Option<&str>) -> Result<()> {
    download_with(url, dest, sha256, net::open_stream, free_space)
}

fn download_with(
    url: &str,
    dest: &Path,
    sha256: Option<&str>,
    open: impl Fn(&str, u64) -> Result<Stream>,
    free: impl FnOnce(&Path) -> Option<u64>,
) -> Result<()> {
    let part = part_of(dest);
    let mut resume_from = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut r = open(url, resume_from)?;
    if r.status == 416 && resume_from > 0 {
        if r.range_total == Some(resume_from) {
            println!("    already complete");
            return finish(&part, dest, sha256, Some(resume_from));
        }
        // Longer than the file now is: left over from another build of a
        // file that keeps its name (Arch /latest/, HBCD). Start over.
        println!("    partial download is from another build -- starting over");
        resume_from = 0;
        r = open(url, 0)?;
    }
    if r.status >= 400 {
        bail!("HTTP Error {}", r.status);
    }
    let host = if r.host.is_empty() {
        net::host_of(url)
    } else {
        r.host.clone()
    };
    if r.content_type.to_lowercase().contains("html") {
        bail!("server returned an HTML page, not a file ({host})");
    }
    let (mut done, total, append);
    if r.status == 206 && resume_from > 0 {
        (done, total, append) = (resume_from, r.range_total, true);
        let msg = format!("server: {host}  (resuming at {})", fmt_size(done));
        println!("    {}", dim(&msg));
    } else {
        resume_from = 0;
        (done, total, append) = (0, r.content_length, false);
        println!("    {}", dim(&format!("server: {host}")));
    }

    // Refuse before writing instead of failing at the last gigabyte: a .part
    // that filled the stick takes every later download down with it. An
    // existing .part does not count against the space -- it is either resumed
    // or overwritten. Unknown free space lets the write decide.
    let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if let (Some(t), Some(f)) = (
        total.filter(|&t| t > 0),
        free(part.parent().unwrap_or(Path::new(""))),
    ) {
        let need = t.saturating_sub(have);
        if need > f {
            bail!(
                "not enough free space: {} to write, {} free",
                fmt_size(need),
                fmt_size(f)
            );
        }
    }

    let start = Instant::now();
    let (mut last_t, mut last_b) = (start, done);
    {
        let mut f = if append {
            OpenOptions::new().append(true).open(&part)?
        } else {
            File::create(&part)?
        };
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = r.reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n])?;
            done += n as u64;
            let dt = last_t.elapsed().as_secs_f64();
            if dt >= 0.5 {
                progress(done, total, (done - last_b) as f64 / dt, false);
                (last_t, last_b) = (Instant::now(), done);
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64().max(1e-6);
    progress(done, total, (done - resume_from) as f64 / elapsed, true);
    finish(&part, dest, sha256, total)
}

/// Size and SHA-256 check of a finished .part, then the rename onto `dest`.
fn finish(part: &Path, dest: &Path, sha256: Option<&str>, total: Option<u64>) -> Result<()> {
    let got = fs::metadata(part)?.len();
    if let Some(t) = total
        && got != t
    {
        bail!(
            "incomplete download: got {} of {}",
            fmt_size(got),
            fmt_size(t)
        );
    }
    if let Some(want) = sha256 {
        print!("    verifying sha256 ...");
        let _ = io::stdout().flush();
        let actual = net::sha256_file(part)?;
        if actual != want {
            let _ = fs::remove_file(part);
            bail!(
                "sha256 mismatch (expected {}..., got {}...)",
                &want[..want.len().min(12)],
                &actual[..actual.len().min(12)]
            );
        }
        println!(" ok");
    }
    fs::rename(part, dest)?;
    Ok(())
}

/// Move a file, with a progress line whenever it has to be copied.
///
/// Moving onto the stick crosses filesystems, where a move degrades to a silent
/// copy -- minutes of a frozen-looking screen for a multi-GB ISO. A same-volume
/// move stays an instant rename; a real copy goes through a .part so an
/// interrupted copy never leaves half an ISO under the final name.
pub fn move_with_progress(src: &Path, dst: &Path) -> Result<()> {
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    let total = fs::metadata(src)?.len();
    let part = part_of(dst);
    let _temp = Temp::new(&part);
    println!(
        "    copying {} to {}",
        fmt_size(total),
        dst.parent().unwrap_or(Path::new("")).display()
    );
    let copy = || -> Result<()> {
        let (mut fi, mut fo) = (File::open(src)?, File::create(&part)?);
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        let (mut done, start) = (0u64, Instant::now());
        let (mut last_t, mut last_b) = (start, 0u64);
        loop {
            let n = fi.read(&mut buf)?;
            if n == 0 {
                break;
            }
            fo.write_all(&buf[..n])?;
            done += n as u64;
            let dt = last_t.elapsed().as_secs_f64();
            if dt >= 0.5 {
                progress(done, Some(total), (done - last_b) as f64 / dt, false);
                (last_t, last_b) = (Instant::now(), done);
            }
        }
        progress(
            done,
            Some(total),
            done as f64 / start.elapsed().as_secs_f64().max(1e-6),
            true,
        );
        if done != total {
            bail!("incomplete copy: {} of {}", fmt_size(done), fmt_size(total));
        }
        drop(fo);
        fs::rename(&part, dst)?;
        Ok(())
    };
    if let Err(e) = copy() {
        // A stray .part on the stick is pure confusion.
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::remove_file(src)?;
    Ok(())
}

/// Download the MemTest86 zip and extract memtest86-usb.img into dest_dir.
pub fn download_memtest(dest_dir: &Path) -> Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("memtest_{}_{nanos}", std::process::id()));
    fs::create_dir_all(&tmp)?;
    let _temp = Temp::new(&tmp);
    let zpath = tmp.join("memtest86-usb.zip");
    let res = download(
        "https://www.memtest86.com/downloads/memtest86-usb.zip",
        &zpath,
        None,
    )
    .and_then(|()| extract_img(&zpath, &dest_dir.join("memtest86-usb.img")));
    let _ = fs::remove_dir_all(&tmp);
    res
}

/// Unpacked straight onto the stick: the image is 1 GiB, and going through the
/// temp dir first writes all of it twice. The .part keeps a half-written image
/// (or one failing the CRC check) from ever carrying the final name.
fn extract_img(zpath: &Path, out: &Path) -> Result<()> {
    let part = part_of(out);
    let _temp = Temp::new(&part);
    let res = (|| -> Result<()> {
        let mut z = zip::ZipArchive::new(File::open(zpath)?)?;
        let idx = (0..z.len()).find(|&i| {
            z.by_index_raw(i)
                .is_ok_and(|f| f.name_raw().to_ascii_lowercase().ends_with(b".img"))
        });
        let Some(idx) = idx else {
            bail!("no .img in {}", zpath.display())
        };
        let mut src = z.by_index(idx)?;
        io::copy(&mut src, &mut File::create(&part)?)?;
        fs::rename(&part, out)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&part);
    }
    res
}

/// When the stick has no room for the new version next to the old one, the
/// old one goes first: ISOs can be fetched again, and a stick filled to the
/// brim would otherwise never update. A same-name update counts too, since the
/// .part grows next to the file it replaces.
fn make_room_for_update(
    dest: &Path,
    old: Option<&str>,
    path: &Path,
    remote: Option<u64>,
    free: Option<u64>,
) -> Result<()> {
    let (Some(old), Some(size), Some(free)) = (old, remote, free) else {
        return Ok(());
    };
    let old_path = dest.join(old);
    let Ok(old_md) = fs::metadata(&old_path) else {
        return Ok(());
    };
    let have = fs::metadata(part_of(path)).map(|m| m.len()).unwrap_or(0);
    let need = size.saturating_sub(have);
    if need > free && need <= free + old_md.len() {
        println!(
            "    {}",
            dim(&format!(
                "no room for both versions -- removing old {old} first"
            ))
        );
        fs::remove_file(&old_path)?;
    }
    Ok(())
}

/// Update-check -> (mirror race) -> checksum -> download -> record.
/// Under --dry-run it stops after the check and reports what it would do.
/// `checked` is a check_update() result the caller already has.
#[allow(clippy::too_many_arguments)]
pub fn fetch_tracked(
    dest: &Path,
    manifest: &mut Manifest,
    key: &str,
    label: &str,
    url: &str,
    name: &str,
    mirrors: &[String],
    args: &Args,
    summary: &mut Summary,
    checked: Option<Check>,
) {
    if let Err(e) = check_name(name) {
        println!("    {}", red(&format!("{label}: FAILED: {e}")));
        summary.push((name.into(), format!("FAIL: {e}")));
        return;
    }
    let old = manifest::entry_filename(manifest, key).map(str::to_owned);
    let Check { need, meta, .. } = if args.force {
        Check {
            need: true,
            reason: "forced".into(),
            meta: None,
        }
    } else {
        checked.unwrap_or_else(|| manifest::check_update(dest, manifest.get(key), url, name))
    };
    if args.dry_run {
        // Nothing is written in a dry run, manifest included.
        summary.push((
            name.into(),
            if need {
                "WOULD DOWNLOAD"
            } else {
                "SKIP (up to date)"
            }
            .into(),
        ));
        return;
    }
    if !need {
        // Persist the on-disk state so later runs skip instantly, even for files
        // that were already on the stick (adopted, no prior record).
        if old.as_deref() != Some(name) {
            manifest::record_version(dest, manifest, key, url, name, meta.as_ref());
        }
        // A leftover partial from an aborted download is just wasted space now.
        let part = part_of(&dest.join(name));
        if let Ok(md) = fs::metadata(&part)
            && fs::remove_file(&part).is_ok()
        {
            let msg = format!("{label}: removed stale partial ({})", fmt_size(md.len()));
            println!("  {}", dim(&msg));
        }
        summary.push((name.into(), "SKIP (up to date)".into()));
        return;
    }
    let res = (|| -> Result<()> {
        let primary = url;
        let mut url = url.to_string();
        if !mirrors.is_empty() && !args.no_mirror_test {
            let all: Vec<String> = std::iter::once(url.clone())
                .chain(mirrors.iter().cloned())
                .collect();
            url = net::fastest_mirror(&all);
        }
        // The checksum comes from the primary, not from the mirror that won:
        // the mirror lists include third-party hosts, and a sums file next to a
        // tampered image proves nothing. A mirror whose own sums disagree is
        // stale or worse (Arch's /latest/ keeps one filename across releases)
        // and loses its win to the primary.
        let expected = if args.no_verify {
            None
        } else {
            net::remote_sha256(primary, name)
        };
        if let Some(exp) = &expected
            && url != primary
            && net::remote_sha256(&url, name).is_some_and(|theirs| &theirs != exp)
        {
            println!(
                "    {} lists a different checksum (stale?) -- using the primary",
                net::host_of(&url)
            );
            url = primary.to_string();
        }
        let path = dest.join(name);
        // A .part always resumes: for a fixed-name image it is almost always an
        // interrupted attempt at this very build, and throwing it away would
        // restart multi-GB downloads on every rerun.
        // shortcut: a .part from a build that was itself replaced before the
        // resume is not detected unless sums are published (size alone matches);
        // store the remote size/mtime next to the .part if that ever bites.
        make_room_for_update(
            dest,
            old.as_deref(),
            &path,
            meta.as_ref().and_then(|m| m.size),
            free_space(dest),
        )?;
        download(&url, &path, expected.as_deref())?;
        if let Some(old) = old.as_deref().filter(|&o| o != name) {
            let old_path = dest.join(old);
            if old_path.exists() {
                fs::remove_file(old_path)?;
                println!("    {}", dim(&format!("removed old {old}")));
            }
        }
        let how = match (&expected, args.no_verify) {
            (Some(_), _) => "sha256 verified",
            (None, true) => "not verified (--no-verify)",
            (None, false) => "size checked, no published checksum",
        };
        let size = fs::metadata(&path)?.len() as f64 / (1u64 << 30) as f64;
        println!("    {}", green(&format!("done ({size:.2} GiB), {how}")));
        manifest::try_record_version(dest, manifest, key, &url, name, meta.as_ref())?;
        Ok(())
    })();
    match res {
        Ok(()) => summary.push((name.into(), "OK (updated)".into())),
        Err(e) => {
            println!("    {}", red(&format!("FAILED: {e:#}")));
            summary.push((name.into(), format!("FAIL: {e:#}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vi_download_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn stream(body: &'static [u8], status: u16, range_total: Option<u64>) -> Stream {
        Stream {
            status,
            host: "example.invalid".into(),
            content_type: "application/x-iso".into(),
            content_length: Some(body.len() as u64),
            range_total,
            reader: Box::new(body),
        }
    }

    /// The MemTest86 image goes from the zip onto the stick in one piece; a zip
    /// that turns out broken halfway leaves the old image alone, and no .part.
    #[test]
    fn memtest_extract() {
        let d = tmpdir("memtest");
        let (good, bad, image) = (
            d.join("good.zip"),
            d.join("bad.zip"),
            d.join("memtest86-usb.img"),
        );
        let payload = b"new image".repeat(4096);
        {
            let mut z = zip::ZipWriter::new(File::create(&good).unwrap());
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            z.start_file("ReadMe.txt", opts).unwrap();
            z.write_all(b"x").unwrap();
            z.start_file("memtest86-usb.img", opts).unwrap();
            z.write_all(&payload).unwrap();
            z.finish().unwrap();
        }
        // Flip the stored CRC-32 of the image (central directory, last entry).
        let mut data = fs::read(&good).unwrap();
        let at = data.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap() + 16;
        data[at] ^= 0xFF;
        fs::write(&bad, data).unwrap();

        extract_img(&good, &image).unwrap();
        assert_eq!(fs::read(&image).unwrap(), payload);
        assert!(
            extract_img(&bad, &image).is_err(),
            "a zip with a wrong CRC was accepted"
        );
        assert_eq!(
            fs::read(&image).unwrap(),
            payload,
            "a broken zip replaced the good image"
        );
        assert!(!part_of(&image).exists(), ".part left on the stick");
        fs::remove_dir_all(d).unwrap();
    }

    /// download() refuses what cannot fit, before it writes anything, and counts
    /// a .part being resumed as space already taken care of.
    #[test]
    fn free_space_and_resume() {
        let d = tmpdir("free");
        let dest = d.join("x.iso");
        fs::write(&dest, b"old").unwrap();
        let err = download_with(
            "u",
            &dest,
            None,
            |_, _| Ok(stream(b"0123456789", 200, None)),
            |_| Some(9),
        )
        .unwrap_err();
        assert!(err.to_string().contains("free space"), "wrong error: {err}");
        assert!(
            !part_of(&dest).exists(),
            "a .part was created although nothing fits"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"old");

        // 6 of 10 bytes are already in the .part: 4 to go, 5 free -> fits.
        fs::write(part_of(&dest), b"012345").unwrap();
        download_with(
            "u",
            &dest,
            None,
            |_, off| {
                assert_eq!(off, 6);
                Ok(Stream {
                    content_length: Some(4),
                    ..stream(b"6789", 206, Some(10))
                })
            },
            |_| Some(5),
        )
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"0123456789");

        // A 200 to a range request restarts from scratch; 416 means already complete.
        fs::write(part_of(&dest), b"junk").unwrap();
        download_with(
            "u",
            &dest,
            None,
            |_, _| Ok(stream(b"abc", 200, None)),
            |_| None,
        )
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        fs::write(part_of(&dest), b"done").unwrap();
        download_with(
            "u",
            &dest,
            None,
            |_, _| Ok(stream(b"", 416, Some(4))),
            |_| None,
        )
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"done");

        // HTML error pages and wrong sizes never reach the final name.
        let html = |_: &str, _| {
            Ok(Stream {
                content_type: "text/html".into(),
                ..stream(b"<html>", 200, None)
            })
        };
        assert!(download_with("u", &dest, None, html, |_| None).is_err());
        let short = |_: &str, _| {
            Ok(Stream {
                content_length: Some(99),
                ..stream(b"abc", 200, None)
            })
        };
        assert!(download_with("u", &dest, None, short, |_| None).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"done");
        fs::remove_dir_all(d).unwrap();
    }

    /// A .part longer than the file now is (another build under the same name)
    /// restarts from scratch; a complete one still has to pass the sha256 check.
    #[test]
    fn stale_part_is_not_trusted() {
        let d = tmpdir("stale");
        let dest = d.join("archlinux-x86_64.iso");
        fs::write(&dest, b"good old image").unwrap();
        fs::write(part_of(&dest), b"stale bytes of last month's build").unwrap();
        let open = |_: &str, off: u64| {
            Ok(if off > 0 {
                stream(b"", 416, Some(10))
            } else {
                stream(b"0123456789", 200, None)
            })
        };
        download_with("u", &dest, None, open, |_| None).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"0123456789");

        fs::write(part_of(&dest), b"0123456789").unwrap();
        let wrong = "00".repeat(32);
        let open = |_: &str, _| Ok(stream(b"", 416, Some(10)));
        assert!(download_with("u", &dest, Some(&wrong), open, |_| None).is_err());
        assert!(!part_of(&dest).exists(), "a .part failing sha256 was kept");
        assert_eq!(fs::read(&dest).unwrap(), b"0123456789");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn upstream_names_stay_in_dest() {
        for bad in ["a/../../x.iso", r"a\..\x.iso", "C:x.iso", "..", ""] {
            assert!(check_name(bad).is_err(), "{bad:?} accepted");
        }
        check_name("ubuntu-26.04.1-desktop-amd64.iso").unwrap();

        // fetch_tracked refuses before touching anything (no manifest, no .part removal).
        let d = tmpdir("traversal");
        let args = Args::parse_from(["x"]);
        let (mut m, mut summary) = (Manifest::new(), Vec::new());
        let up_to_date = Check {
            need: false,
            reason: "up to date".into(),
            meta: None,
        };
        fetch_tracked(
            &d,
            &mut m,
            "k",
            "K",
            "u",
            "../evil.iso",
            &[],
            &args,
            &mut summary,
            Some(up_to_date),
        );
        assert!(summary[0].1.starts_with("FAIL"), "{summary:?}");
        assert!(m.is_empty() && !d.join(crate::MANIFEST_NAME).exists());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn temps_unregister_on_drop() {
        let p = PathBuf::from("/nonexistent/vi-temp-probe");
        let t = Temp::new(&p);
        assert!(TEMPS.lock().unwrap().contains(&p));
        drop(t);
        assert!(!TEMPS.lock().unwrap().contains(&p));
    }

    #[test]
    fn free_space_reports_something() {
        assert!(free_space(&std::env::temp_dir()).is_some_and(|n| n > 0));
    }

    /// The old version is removed only when that is what makes the new one fit.
    #[test]
    fn old_version_goes_first_only_when_needed() {
        let d = tmpdir("makeroom");
        let old = d.join("x-1.iso");
        let new = d.join("x-2.iso");
        let reset = || fs::write(&old, vec![0u8; 100]).unwrap();
        reset();
        // Enough room for both: keep the old one until the new one is in.
        make_room_for_update(&d, Some("x-1.iso"), &new, Some(150), Some(200)).unwrap();
        assert!(old.exists());
        // Only fits once the old one is gone.
        make_room_for_update(&d, Some("x-1.iso"), &new, Some(150), Some(60)).unwrap();
        assert!(!old.exists());
        reset();
        // Does not fit even then: deleting would only lose the old one.
        make_room_for_update(&d, Some("x-1.iso"), &new, Some(500), Some(60)).unwrap();
        assert!(old.exists());
        // A .part already on disk counts as written.
        fs::write(part_of(&new), vec![0u8; 100]).unwrap();
        make_room_for_update(&d, Some("x-1.iso"), &new, Some(150), Some(60)).unwrap();
        assert!(old.exists());
        // Unknown remote size or free space: never guess.
        make_room_for_update(&d, Some("x-1.iso"), &new, None, Some(0)).unwrap();
        make_room_for_update(&d, Some("x-1.iso"), &new, Some(150), None).unwrap();
        assert!(old.exists());
        fs::remove_dir_all(&d).unwrap();
    }
}
