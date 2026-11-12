//! HTTP helpers, checksums, mirror speed test, listing helpers
//! (Python: _auth_for .. latest_in_listing, lines ~140-435).

use std::io::Read;
use std::path::Path;

use anyhow::Result;

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

/// Set on Ctrl+C: queued lookups give up instead of sending their request.
pub fn abort() { todo!() }
pub fn aborted() -> bool { todo!() }

/// GET a page as text, one request per host at a time, 45 s timeout.
pub fn http_get(url: &str) -> Result<String> { http_get_timeout(url, 45) }
pub fn http_get_timeout(_url: &str, _secs: u64) -> Result<String> { todo!() }
/// http_get, but each URL fetched only once per run.
pub fn http_get_cached(_url: &str) -> Result<String> { todo!() }
/// Open a download, optionally resuming at `offset` (Range header).
pub fn open_stream(_url: &str, _offset: u64) -> Result<Stream> { todo!() }
pub fn remote_meta(_url: &str) -> Meta { todo!() }
pub fn remote_sha256(_url: &str, _filename: &str) -> Option<String> { todo!() }
pub fn sha256_file(_path: &Path) -> std::io::Result<String> { todo!() }
pub fn fmt_size(_n: u64) -> String { todo!() }
pub fn fastest_mirror(_urls: &[String]) -> String { todo!() }
/// Sort key from the dotted version runs in a name.
pub fn verkey(_name: &str) -> Vec<Vec<u64>> { todo!() }
/// (url, filename) of the highest-versioned regex match (group 1) in an autoindex.
pub fn latest_in_listing(_base: &str, _file_regex: &str) -> Result<(String, String)> { todo!() }
pub fn urljoin(_base: &str, _rel: &str) -> String { todo!() }
pub fn host_of(_url: &str) -> String { todo!() }
