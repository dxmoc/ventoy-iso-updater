//! HTTP helpers, checksums, mirror speed test, listing helpers
//! (Python: _auth_for .. latest_in_listing, lines ~140-435).

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use regex::Regex;
use sha2::{Digest, Sha256};
use ureq::ResponseExt;
use ureq::http::Response;

/// Remote file metadata (Python: remote_meta() -> (size, last_modified)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub size: Option<u64>,
    pub mtime: Option<String>,
}

/// An open download response (for download::download).
pub struct Stream {
    pub status: u16,
    /// Host that actually serves the bytes, after redirects.
    pub host: String,
    pub content_type: String,
    pub content_length: Option<u64>,
    /// Full size from Content-Range, if the server answered a range request.
    pub range_total: Option<u64>,
    pub reader: Box<dyn Read + Send>,
}

/// Browser-like UA for everything but SourceForge (version = crate::VERSION).
pub const UA: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) VentoyISOUpdater/",
    env!("CARGO_PKG_VERSION")
);

type Body = Response<ureq::Body>;

/// TLS checks against the OS trust store, like Python's ssl module did: antivirus
/// HTTPS scanning and corporate proxies install their root there.
pub(crate) static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .build()
        .new_agent()
});
static ABORT: AtomicBool = AtomicBool::new(false);
static HOST_LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(Default::default);
static LISTING_CACHE: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

/// Set on Ctrl+C: queued lookups give up instead of sending their request.
pub fn abort() {
    ABORT.store(true, Ordering::SeqCst)
}
pub fn aborted() -> bool {
    ABORT.load(Ordering::SeqCst)
}

/// HTTP status of a failed request, if the error was one (e.g. 404, 416).
pub fn http_status(e: &anyhow::Error) -> Option<u16> {
    match e.downcast_ref::<ureq::Error>() {
        Some(ureq::Error::StatusCode(code)) => Some(*code),
        _ => None,
    }
}

/// Bearer token for the GitHub API when GITHUB_TOKEN / GH_TOKEN is set.
///
/// Unauthenticated the API allows 60 requests an hour per IP: fine from home,
/// not from shared CI runners where someone else already spent that budget.
fn auth_for(url: &str) -> Option<String> {
    let token = ["GITHUB_TOKEN", "GH_TOKEN"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|t| !t.is_empty())?;
    (host_of(url).to_lowercase() == "api.github.com").then(|| format!("Bearer {token}"))
}

/// SourceForge serves a browser UA an HTML "your download will start" page and
/// only redirects wget/curl to the binary, so it must not look like a browser.
pub fn ua_for(url: &str) -> &'static str {
    let host = host_of(url).to_lowercase();
    if host.contains("sourceforge") || host.contains("sf.net") {
        "Wget/1.21"
    } else {
        UA
    }
}

/// Run `f` once no other request to the same host is in flight.
///
/// Resolvers run on threads and many entries share a host (four Proxmox images,
/// ten Ubuntu flavours, all of SourceForge); enterprise.proxmox.com answers four
/// simultaneous requests by timing out the TLS handshake. Not re-entrant: callers
/// that fetch under the lock use the unlocked `fetch`. After Ctrl+C whatever is
/// still queued gives up instead of sending its request.
fn host_turn<T>(url: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock = HOST_LOCKS
        .lock()
        .unwrap()
        .entry(host_of(url))
        .or_default()
        .clone();
    let _turn = lock.lock().unwrap_or_else(|p| p.into_inner());
    if aborted() {
        bail!("aborted");
    }
    f()
}

/// One request with urllib-like timeouts: `secs` for connecting and per read.
/// 4xx/5xx are `ureq::Error::StatusCode`, except 416 when `allow_416`.
fn send(
    url: &str,
    head: bool,
    headers: &[(&str, &str)],
    secs: u64,
    allow_416: bool,
) -> Result<Body, ureq::Error> {
    let t = Some(Duration::from_secs(secs));
    let mut req = if head {
        AGENT.head(url)
    } else {
        AGENT.get(url)
    };
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let r = req
        .config()
        .timeout_connect(t)
        .timeout_per_read(t)
        .http_status_as_error(false)
        .build()
        .call()?;
    match r.status().as_u16() {
        416 if allow_416 => Ok(r),
        code if code >= 400 => Err(ureq::Error::StatusCode(code)),
        _ => Ok(r),
    }
}

fn header(r: &Body, name: &str) -> String {
    r.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Full size from a "bytes 0-0/12345" Content-Range.
fn range_total(cr: &str) -> Option<u64> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/(\d+)\s*$").unwrap());
    RE.captures(cr)?[1].parse().ok()
}

fn fetch(url: &str, secs: u64) -> Result<String> {
    let mut headers = vec![("User-Agent", UA)];
    let auth = auth_for(url);
    if let Some(a) = &auth {
        headers.push(("Authorization", a));
    }
    let mut r = send(url, false, &headers, secs, false)?;
    // Pages, JSON and sums files are small; a server answering a sums URL with
    // the image itself would otherwise pull gigabytes into memory.
    let bytes = r.body_mut().with_config().limit(16 << 20).read_to_vec()?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// GET a page as text, one request per host at a time, 45 s timeout.
pub fn http_get(url: &str) -> Result<String> {
    http_get_timeout(url, 45)
}
pub fn http_get_timeout(url: &str, secs: u64) -> Result<String> {
    host_turn(url, || fetch(url, secs))
}

/// http_get, but each URL fetched only once per run.
///
/// Several entries resolve off one page (four Proxmox products, three Fedora
/// editions). Runs are short, so a value never goes stale. Looked up under the
/// host lock, so threads asking at once wait for the first fetch.
pub fn http_get_cached(url: &str) -> Result<String> {
    http_get_cached_timeout(url, 45)
}
pub fn http_get_cached_timeout(url: &str, secs: u64) -> Result<String> {
    host_turn(url, || {
        if let Some(hit) = LISTING_CACHE.lock().unwrap().get(url) {
            return Ok(hit.clone());
        }
        let text = fetch(url, secs)?;
        LISTING_CACHE
            .lock()
            .unwrap()
            .insert(url.to_string(), text.clone());
        Ok(text)
    })
}

/// Open a download, optionally resuming at `offset` (Range header).
/// 416 (the .part is already complete) comes back as Ok with status 416; other
/// 4xx/5xx are errors whose code `http_status` reads.
pub fn open_stream(url: &str, offset: u64) -> Result<Stream> {
    let range = format!("bytes={offset}-");
    let mut headers = vec![("User-Agent", ua_for(url)), ("Accept-Encoding", "identity")];
    if offset > 0 {
        headers.push(("Range", &range));
    }
    let r = send(url, false, &headers, 60, true)?;
    let host = r
        .get_uri()
        .authority()
        .map(|a| a.to_string())
        .unwrap_or_else(|| host_of(url));
    Ok(Stream {
        status: r.status().as_u16(),
        host,
        content_type: header(&r, "Content-Type"),
        content_length: header(&r, "Content-Length").parse().ok(),
        range_total: range_total(&header(&r, "Content-Range")),
        reader: Box::new(r.into_body().into_reader()),
    })
}

/// Best-effort (size, last_modified) of a remote file, following redirects.
///
/// A 1-byte ranged GET first: it forces redirect chains (SourceForge, mirrors)
/// to resolve to the real binary and exposes the full size via Content-Range.
/// HEAD if that fails. An HTML answer is an interstitial page, not the file.
pub fn remote_meta(url: &str) -> Meta {
    let ua = ua_for(url);
    let read = |head: bool| -> Result<Meta> {
        let mut headers = vec![("User-Agent", ua), ("Accept-Encoding", "identity")];
        if !head {
            headers.push(("Range", "bytes=0-0"));
        }
        host_turn(url, || {
            let r = send(url, head, &headers, 30, false)?;
            if header(&r, "Content-Type").to_lowercase().contains("html") {
                bail!("html");
            }
            let size = range_total(&header(&r, "Content-Range"))
                .or_else(|| header(&r, "Content-Length").parse().ok());
            let mtime = Some(header(&r, "Last-Modified")).filter(|s| !s.is_empty());
            Ok(Meta { size, mtime })
        })
    };
    read(false).or_else(|_| read(true)).unwrap_or_default()
}

/// Suffixes for a sums file that belongs to one image, appended to its URL.
pub const PER_FILE_SUMS: &[&str] = &[".sha256", ".sha256sum", ".DIGESTS", ".sha256.txt"];
/// Names of a sums file covering a whole directory, resolved next to the image.
pub const DIR_SUMS: &[&str] = &[
    "SHA256SUMS",
    "sha256sum.txt",
    "sha256sums.txt",
    "CHECKSUM",
    "SHA256SUM",
];

const HASH: &str = "[0-9a-fA-F]{64}";

/// SHA-256 for `filename` from a sums file: coreutils ("<hash>  [*]<file>")
/// or BSD ("SHA256 (<file>) = <hash>") form.
pub fn sha256_from_text(text: &str, filename: &str) -> Option<String> {
    let f = regex::escape(filename);
    [
        format!(r"(?m)^({HASH})[ \t]+[*]?{f}[ \t]*$"),
        format!(r"SHA256\s*\(\s*{f}\s*\)\s*=\s*({HASH})"),
    ]
    .iter()
    .find_map(|p| {
        Regex::new(p)
            .ok()?
            .captures(text)
            .map(|c| c[1].to_lowercase())
    })
}

/// Best-effort SHA-256 for an image, from whatever the source publishes.
/// None when nothing is (SourceForge, vendor one-offs): an unverified download
/// still beats no download.
pub fn remote_sha256(url: &str, filename: &str) -> Option<String> {
    remote_sha256_with(&|u| http_get_timeout(u, 20), url, filename)
}

/// remote_sha256 with the fetcher passed in (tests use fake pages).
///
/// A per-image sums file is addressed by the image URL, so a single hash in it
/// counts even under another name (NixOS writes the upstream build name). It
/// must be exactly one, standing between whitespace: that keeps a SHA-512 from
/// reading as a SHA-256, and keeps out the HTML page SourceForge answers with,
/// whose icon URLs once made a good download fail as a "mismatch".
pub fn remote_sha256_with(
    get: &dyn Fn(&str) -> Result<String>,
    url: &str,
    filename: &str,
) -> Option<String> {
    for suffix in PER_FILE_SUMS {
        let Ok(text) = get(&format!("{url}{suffix}")) else {
            continue;
        };
        if let Some(h) = sha256_from_text(&text, filename) {
            return Some(h);
        }
        let lone: Vec<&str> = text
            .split_whitespace()
            .filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()))
            .collect();
        if lone.len() == 1 {
            return Some(lone[0].to_lowercase());
        }
    }
    let base = &url[..url.rfind('/').map_or(0, |i| i + 1)];
    DIR_SUMS
        .iter()
        .find_map(|c| sha256_from_text(&get(&format!("{base}{c}")).ok()?, filename))
}

/// SHA-256 of a file, streamed so a 5 GB ISO stays out of memory.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let (mut h, mut f) = (Sha256::new(), std::fs::File::open(path)?);
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        match f.read(&mut buf)? {
            0 => break,
            n => h.update(&buf[..n]),
        }
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Human-readable byte count (binary units).
pub fn fmt_size(n: u64) -> String {
    let mut x = n as f64;
    for unit in ["B", "KiB", "MiB", "GiB"] {
        if x < 1024.0 {
            return format!("{x:.1} {unit}");
        }
        x /= 1024.0;
    }
    format!("{x:.1} TiB")
}

const SPEEDTEST_SECONDS: f64 = 2.0; // measured window per mirror
const SPEEDTEST_WARMUP: u64 = 512 * 1024; // bytes discarded before the clock starts

/// A mirror's throughput in bytes/sec over a fixed window.
///
/// The clock starts after the warmup so slow-start and the TLS handshake don't
/// drag the result down. The window is capped at ~80 MB (~320 Mbit/s for the
/// full duration) so picking a mirror never costs more traffic than it saves.
fn measure_speed(url: &str) -> f64 {
    let timeout = 8.0;
    let cap = (SPEEDTEST_SECONDS * 40.0 * 1024.0 * 1024.0) as u64 + SPEEDTEST_WARMUP;
    let range = format!("bytes=0-{}", cap - 1);
    let headers = [
        ("User-Agent", ua_for(url)),
        ("Range", range.as_str()),
        ("Accept-Encoding", "identity"),
    ];
    let started = Instant::now();
    let Ok(r) = send(url, false, &headers, timeout as u64, false) else {
        return 0.0;
    };
    let mut reader = r.into_body().into_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let (mut warm, mut got, mut t0) = (0u64, 0u64, None::<Instant>);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n as u64,
            Err(_) => return 0.0,
        };
        match t0 {
            None => {
                warm += n;
                if warm >= SPEEDTEST_WARMUP {
                    t0 = Some(Instant::now());
                } else if started.elapsed().as_secs_f64() > timeout {
                    break;
                }
            }
            Some(t) => {
                got += n;
                if t.elapsed().as_secs_f64() >= SPEEDTEST_SECONDS {
                    break;
                }
            }
        }
    }
    // Never got through the warmup: a slow-but-alive host still beats a dead one.
    let (bytes, dt) = match t0 {
        None => (warm, started.elapsed().as_secs_f64()),
        Some(t) => (got, t.elapsed().as_secs_f64()),
    };
    if bytes > 0 && dt > 0.0 {
        bytes as f64 / dt
    } else {
        0.0
    }
}

/// Speed-test each candidate in turn (in parallel they would compete for the
/// same bandwidth) and return the fastest; the first if none responds.
pub fn fastest_mirror(urls: &[String]) -> String {
    let mut seen = BTreeSet::new();
    let urls: Vec<&String> = urls
        .iter()
        .filter(|u| !u.is_empty() && seen.insert(*u))
        .collect();
    if urls.len() <= 1 {
        return urls.first().map(|u| u.to_string()).unwrap_or_default();
    }
    println!(
        "    speed-testing {} mirrors (up to {SPEEDTEST_SECONDS:.0}s each)...",
        urls.len()
    );
    let (mut best, mut best_bps) = (urls[0], -1.0);
    for u in urls {
        let bps = measure_speed(u);
        let host = host_of(u);
        if bps > 0.0 {
            println!("      {host:<42} {:>6.1} Mbit/s", bps * 8.0 / 1e6);
        } else {
            println!("      {host:<42} unreachable");
        }
        if bps > best_bps {
            (best, best_bps) = (u, bps);
        }
    }
    println!("    -> fastest: {}", host_of(best));
    best.clone()
}

/// Sort key from the dotted version runs in a name, so 13.01 > 12.03.
///
/// Each dotted run is its own sub-list. A flat list of every integer let what
/// follows the version decide: "ubuntu-26.04-desktop-amd64" gave (26, 4, 64),
/// beating (26, 4, 1, 64) from 26.04.1 because 64 > 1.
pub fn verkey(name: &str) -> Vec<Vec<u64>> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9]+(?:\.[0-9]+)*").unwrap());
    RE.find_iter(name)
        .map(|m| {
            m.as_str()
                .split('.')
                .map(|n| n.parse().unwrap_or(u64::MAX))
                .collect()
        })
        .collect()
}

/// Group 1 (or the whole match) of every `re` match in `text`.
pub fn find_all(re: &str, text: &str) -> Result<Vec<String>> {
    let re = Regex::new(re)?;
    Ok(re
        .captures_iter(text)
        .map(|c| {
            c.get(1)
                .or_else(|| c.get(0))
                .map_or("", |m| m.as_str())
                .to_string()
        })
        .collect())
}

/// (url, filename) of the highest-versioned file in an already fetched listing.
pub fn pick_latest(base: &str, html: &str, file_regex: &str) -> Result<(String, String)> {
    let files: BTreeSet<String> = find_all(file_regex, html)?.into_iter().collect();
    let Some(best) = files.into_iter().max_by_key(|f| verkey(f)) else {
        bail!("no file matching {file_regex:?} at {base}");
    };
    Ok((urljoin(base, &best), best))
}

/// (url, filename) of the highest-versioned regex match (group 1) in an autoindex.
pub fn latest_in_listing(base: &str, file_regex: &str) -> Result<(String, String)> {
    pick_latest(base, &http_get_cached(base)?, file_regex)
}

/// RFC 3986 reference resolution, as far as listing hrefs need it.
pub fn urljoin(base: &str, rel: &str) -> String {
    if rel.contains("://") {
        return rel.to_string();
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let auth_end = base[scheme_end..]
        .find('/')
        .map_or(base.len(), |i| scheme_end + i);
    if let Some(r) = rel.strip_prefix("//") {
        return format!("{}{r}", &base[..scheme_end]);
    }
    let path = if rel.starts_with('/') {
        rel.to_string()
    } else {
        let base_path = base[auth_end..].split(['?', '#']).next().unwrap_or("");
        let dir = &base_path[..base_path.rfind('/').map_or(0, |i| i + 1)];
        format!("{}{rel}", if dir.is_empty() { "/" } else { dir })
    };
    // Drop "." and resolve ".." segments.
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    for (i, seg) in segs.iter().enumerate() {
        let last = i == segs.len() - 1;
        match *seg {
            "." => {
                if last {
                    out.push("")
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    format!("{}{}", &base[..auth_end], out.join("/"))
}

/// Network location (Python: urlparse(url).netloc).
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verkey_point_release_beats_base() {
        assert!(
            verkey("ubuntu-26.04.1-desktop-amd64.iso") > verkey("ubuntu-26.04-desktop-amd64.iso")
        );
        assert!(
            verkey("ubuntu-24.04.5.1-desktop-amd64.iso")
                > verkey("ubuntu-24.04.5-desktop-amd64.iso")
        );
        assert!(
            verkey("linuxmint-22.1-cinnamon-64bit.iso") > verkey("linuxmint-22-cinnamon-64bit.iso")
        );
        assert!(verkey("x-13.01.iso") > verkey("x-12.03.iso"));
        assert_eq!(
            verkey("ubuntu-26.04-desktop-amd64"),
            vec![vec![26, 4], vec![64]]
        );
    }

    #[test]
    fn sums_parsing() {
        let h = "AB".repeat(32);
        let lower = h.to_lowercase();
        assert_eq!(
            sha256_from_text(&format!("x\n{h}  a.iso\n"), "a.iso"),
            Some(lower.clone())
        );
        assert_eq!(
            sha256_from_text(&format!("{h} *a.iso"), "a.iso"),
            Some(lower.clone())
        );
        assert_eq!(
            sha256_from_text(&format!("SHA256 (a.iso) = {h}"), "a.iso"),
            Some(lower.clone())
        );
        assert_eq!(
            sha256_from_text(&format!("{h}  a.iso.torrent"), "a.iso"),
            None
        );
        assert_eq!(sha256_from_text(&format!("{h}  aXiso"), "a.iso"), None);
    }

    #[test]
    fn sums_ignore_html_but_read_lone_hash() {
        let digest = "ab".repeat(32);
        let html = format!(
            r#"<html><img src="//a.example/icon?{}?w=48">"#,
            "cd".repeat(32)
        );
        let get = |u: &str| -> Result<String> {
            match u {
                "https://example.invalid/a.iso.sha256" => Ok(html.clone()),
                "https://example.invalid/b.iso.sha256" => {
                    Ok(format!("{digest}  upstream-build-name.iso\n"))
                }
                _ => Err(ureq::Error::StatusCode(404).into()),
            }
        };
        assert_eq!(
            remote_sha256_with(&get, "https://example.invalid/a.iso", "a.iso"),
            None
        );
        assert_eq!(
            remote_sha256_with(&get, "https://example.invalid/b.iso", "b.iso"),
            Some(digest.clone())
        );
        // a SHA-512 is not a SHA-256
        let get512 = |_: &str| -> Result<String> { Ok(format!("{}  c.iso", "ab".repeat(64))) };
        assert_eq!(
            remote_sha256_with(&get512, "https://e.invalid/c.iso", "c.iso"),
            None
        );
    }

    #[test]
    fn urljoin_cases() {
        let b = "https://h.org/a/b/";
        assert_eq!(urljoin(b, "x.iso"), "https://h.org/a/b/x.iso");
        assert_eq!(urljoin(b, "./x.iso"), "https://h.org/a/b/x.iso");
        assert_eq!(urljoin(b, "../x.iso"), "https://h.org/a/x.iso");
        assert_eq!(urljoin(b, "/x.iso"), "https://h.org/x.iso");
        assert_eq!(
            urljoin("https://h.org/a/idx.html", "x.iso"),
            "https://h.org/a/x.iso"
        );
        assert_eq!(urljoin("https://h.org", "x.iso"), "https://h.org/x.iso");
        assert_eq!(urljoin(b, "//m.org/x.iso"), "https://m.org/x.iso");
        assert_eq!(urljoin(b, "http://m.org/x.iso"), "http://m.org/x.iso");
    }

    #[test]
    fn small_helpers() {
        assert_eq!(
            host_of("https://Api.github.com:443/x?y"),
            "Api.github.com:443"
        );
        assert_eq!(ua_for("https://downloads.sourceforge.net/x"), "Wget/1.21");
        assert_eq!(fmt_size(1536), "1.5 KiB");
        assert_eq!(fmt_size(0), "0.0 B");
        assert_eq!(range_total("bytes 0-0/12345"), Some(12345));
        let (url, name) = pick_latest(
            "https://h.org/d/",
            r#"href="a-1.9.iso" href="a-1.10.iso" href="a-1.2.iso""#,
            r#"href="(a-[\d.]+\.iso)""#,
        )
        .unwrap();
        assert_eq!(
            (url.as_str(), name.as_str()),
            ("https://h.org/d/a-1.10.iso", "a-1.10.iso")
        );
    }
}
