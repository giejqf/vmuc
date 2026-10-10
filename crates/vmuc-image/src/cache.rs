// SPDX-License-Identifier: Apache-2.0
//! Download cache.
//!
//! Every URL gets its own directory under `by-url-sha256/<sha256(url)>/`
//! holding `url` (the URL as text, for humans), `data` (the bytes),
//! `sha256.digest` (the verified digest in `sha256:<hex>` form),
//! `content-type` (what the server said) and `.lock` (an `flock` file so two
//! vmuc processes never download the same URL at once).
//!
//! Downloads stream into `data.partial` in the same directory and are renamed
//! over `data` only after the digest has been checked, so `data` is either
//! absent or complete and verified. `sha256.digest` is written after `data`,
//! so an entry is only reused when both exist.
//!
//! `file://` URLs are served by copying the local file through the same
//! verification path; unit tests use them so no network is needed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::digest::{Digest, DigestWriter};
use crate::sparse::SparseWriter;
use crate::{Error, Result};

const ENTRY_SUBDIR: &str = "by-url-sha256";
const DATA: &str = "data";
const PARTIAL: &str = "data.partial";
const DIGEST: &str = "sha256.digest";
const URL_FILE: &str = "url";
const CONTENT_TYPE: &str = "content-type";
const LOCK: &str = ".lock";
const CHUNK: usize = 1 << 20;

/// An exclusive advisory lock on a file, released on drop.
pub struct DirLock {
    _file: File,
}

impl DirLock {
    /// Create `path` if needed and take `flock(LOCK_EX)` on it, blocking
    /// until any other holder releases it.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: flock on a descriptor we own; no memory is passed.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { _file: file })
    }
}

impl std::fmt::Debug for DirLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirLock")
    }
}

/// The download cache rooted at one directory.
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// A cache rooted at `root`; nothing is created until the first fetch.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The platform default: `~/Library/Caches/vmuc` on macOS, otherwise
    /// `$XDG_CACHE_HOME/vmuc` or `~/.cache/vmuc`. `None` when `$HOME` is unset.
    pub fn default_dir() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if cfg!(target_os = "macos") {
            return home.map(|h| h.join("Library").join("Caches").join("vmuc"));
        }
        match std::env::var_os("XDG_CACHE_HOME") {
            Some(x) if !x.is_empty() => Some(PathBuf::from(x).join("vmuc")),
            _ => home.map(|h| h.join(".cache").join("vmuc")),
        }
    }

    /// The cache rooted at [`Cache::default_dir`].
    pub fn open_default() -> Result<Self> {
        Self::default_dir().map(Self::new).ok_or_else(|| {
            Error::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "HOME is not set; cannot locate the cache directory",
            ))
        })
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The entry directory for `url`.
    pub fn entry_dir(&self, url: &str) -> PathBuf {
        self.root
            .join(ENTRY_SUBDIR)
            .join(Digest::of_bytes(url.as_bytes()).hex())
    }

    /// Return the path of the verified `data` file for `url`, downloading it
    /// first if the cache has no entry (or an entry with another digest).
    ///
    /// `expected` pins the digest; with `None` any cached entry is reused and
    /// a fresh download is accepted as-is. `progress(downloaded, total)` is
    /// called as bytes arrive; `total` is `None` when the server sends no
    /// `Content-Length`. The per-entry lock is held for the whole call.
    pub fn fetch(
        &self,
        url: &str,
        expected: Option<&Digest>,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<PathBuf> {
        let dir = self.entry_dir(url);
        fs::create_dir_all(&dir)?;
        let _lock = DirLock::acquire(&dir.join(LOCK))?;
        let data = dir.join(DATA);

        if let Some(cached) = cached_digest(&dir)? {
            match expected {
                Some(want) if *want != cached => {
                    tracing::info!(url, cached = %cached, wanted = %want, "cache entry stale");
                }
                _ => {
                    tracing::debug!(url, path = %data.display(), "cache hit");
                    return Ok(data);
                }
            }
        }

        let partial = dir.join(PARTIAL);
        let source = Source::parse(url)?;
        tracing::info!(url, "downloading");
        let (actual, content_type) = {
            let out = File::create(&partial)?;
            let mut writer = DigestWriter::new(out);
            let content_type = source.copy_to(&mut writer, &mut progress)?;
            let (file, digest) = writer.finish();
            file.sync_all()?;
            (digest, content_type)
        };

        if let Some(want) = expected
            && *want != actual
        {
            let _ = fs::remove_file(&partial);
            return Err(Error::DigestMismatch {
                url: url.to_owned(),
                expected: *want,
                actual,
            });
        }

        let _ = fs::remove_file(dir.join(DIGEST));
        fs::rename(&partial, &data)?;
        fs::write(dir.join(URL_FILE), format!("{url}\n"))?;
        fs::write(dir.join(CONTENT_TYPE), format!("{content_type}\n"))?;
        fs::write(dir.join(DIGEST), format!("{actual}\n"))?;
        Ok(data)
    }
}

/// The digest recorded for a complete entry, if both `data` and
/// `sha256.digest` exist and the latter parses.
fn cached_digest(dir: &Path) -> Result<Option<Digest>> {
    if !dir.join(DATA).is_file() {
        return Ok(None);
    }
    match fs::read_to_string(dir.join(DIGEST)) {
        Ok(s) => Ok(s.parse().ok()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

enum Source {
    File(PathBuf),
    Http(String),
}

impl Source {
    fn parse(url: &str) -> Result<Self> {
        if let Some(rest) = url.strip_prefix("file://") {
            let path = rest.strip_prefix("localhost").unwrap_or(rest);
            if !path.starts_with('/') {
                return Err(Error::UnsupportedUrl(url.to_owned()));
            }
            return Ok(Self::File(PathBuf::from(path)));
        }
        if url.starts_with("http://") || url.starts_with("https://") {
            return Ok(Self::Http(url.to_owned()));
        }
        Err(Error::UnsupportedUrl(url.to_owned()))
    }

    /// Stream the source into `out`, reporting progress; returns the content type.
    fn copy_to(
        &self,
        out: &mut impl Write,
        progress: &mut impl FnMut(u64, Option<u64>),
    ) -> Result<String> {
        match self {
            Self::File(path) => {
                let file = File::open(path)?;
                let total = Some(file.metadata()?.len());
                copy_with_progress(file, out, total, progress)?;
                Ok("application/octet-stream".to_owned())
            }
            Self::Http(url) => {
                let agent = ureq::Agent::new_with_config(
                    ureq::Agent::config_builder()
                        .user_agent(concat!("vmuc/", env!("CARGO_PKG_VERSION")))
                        .timeout_connect(Some(Duration::from_secs(30)))
                        .timeout_recv_response(Some(Duration::from_secs(60)))
                        .build(),
                );
                let response = agent.get(url).call().map_err(|e| Error::Http {
                    url: url.clone(),
                    source: Box::new(e),
                })?;
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/octet-stream")
                    .to_owned();
                let body = response.into_body();
                let total = body.content_length();
                copy_with_progress(body.into_reader(), out, total, progress)?;
                Ok(content_type)
            }
        }
    }
}

fn copy_with_progress(
    mut reader: impl Read,
    out: &mut impl Write,
    total: Option<u64>,
    progress: &mut impl FnMut(u64, Option<u64>),
) -> io::Result<u64> {
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    progress(0, total);
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    out.flush()?;
    Ok(done)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    Gzip,
    Xz,
}

/// Sniff gzip (`1f 8b`) and xz (`fd 37 7a 58 5a 00`) magic bytes.
fn sniff(path: &Path) -> io::Result<Option<Compression>> {
    let mut head = [0u8; 6];
    let n = File::open(path)?.read(&mut head)?;
    Ok(if n >= 2 && head[..2] == [0x1f, 0x8b] {
        Some(Compression::Gzip)
    } else if n >= 6 && head == [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00] {
        Some(Compression::Xz)
    } else {
        None
    })
}

/// If `path` is gzip- or xz-compressed, decompress it once into a sparse
/// `<name>.raw` beside it and return that path; otherwise return `path`.
///
/// The result is cached: a second call returns the existing `.raw`. A
/// sibling `.lock` file serialises concurrent callers, and the output is
/// written to `<name>.raw.partial` and renamed, so a crash leaves no
/// half-written `.raw`.
pub fn decompress_if_needed(path: &Path) -> Result<PathBuf> {
    let Some(kind) = sniff(path)? else {
        return Ok(path.to_path_buf());
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::UnsupportedUrl(path.display().to_string()))?;
    let raw = dir.join(format!("{name}.raw"));
    let _lock = DirLock::acquire(&dir.join(LOCK))?;
    if raw.is_file() {
        return Ok(raw);
    }
    let partial = dir.join(format!("{name}.raw.partial"));
    tracing::info!(path = %path.display(), ?kind, "decompressing");
    let input = BufReader::with_capacity(CHUNK, File::open(path)?);
    let out = File::create(&partial)?;
    let mut writer = SparseWriter::new(out);
    match kind {
        Compression::Gzip => {
            io::copy(&mut flate2::read::MultiGzDecoder::new(input), &mut writer)
                .map_err(|e| decompress_error(path, &partial, &e))?;
        }
        Compression::Xz => {
            let mut input = input;
            lzma_rs::xz_decompress(&mut input, &mut writer)
                .map_err(|e| decompress_error(path, &partial, &e))?;
        }
    }
    let (file, len) = writer.finish()?;
    file.set_len(len)?;
    file.sync_all()?;
    fs::rename(&partial, &raw)?;
    Ok(raw)
}

fn decompress_error(path: &Path, partial: &Path, e: &dyn std::fmt::Display) -> Error {
    let _ = fs::remove_file(partial);
    Error::Decompress {
        path: path.to_path_buf(),
        reason: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn file_url(p: &Path) -> String {
        format!("file://{}", p.display())
    }

    fn payload() -> Vec<u8> {
        (0..300_000u32).map(|i| (i * 7 % 253) as u8).collect()
    }

    #[test]
    fn default_dir_follows_platform() {
        let d = Cache::default_dir().unwrap();
        if cfg!(target_os = "macos") {
            assert!(d.ends_with("Library/Caches/vmuc"));
        } else {
            assert!(d.ends_with("vmuc"));
        }
    }

    #[test]
    fn entry_dir_is_sha256_of_url() {
        let c = Cache::new("/c");
        let d = c.entry_dir("https://example.com/x.img");
        assert_eq!(
            d,
            PathBuf::from("/c/by-url-sha256/")
                .join(Digest::of_bytes(b"https://example.com/x.img").hex())
        );
        assert_eq!(c.root(), Path::new("/c"));
    }

    #[test]
    fn fetch_file_url_verifies_and_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        let data = payload();
        fs::write(&src, &data).unwrap();
        let digest = Digest::of_bytes(&data);
        let cache = Cache::new(tmp.path().join("cache"));
        let url = file_url(&src);

        let mut calls = Vec::new();
        let p = cache
            .fetch(&url, Some(&digest), |d, t| calls.push((d, t)))
            .unwrap();
        assert_eq!(fs::read(&p).unwrap(), data);
        assert_eq!(calls.first(), Some(&(0, Some(data.len() as u64))));
        assert_eq!(
            calls.last(),
            Some(&(data.len() as u64, Some(data.len() as u64)))
        );

        let dir = cache.entry_dir(&url);
        assert_eq!(p, dir.join("data"));
        assert_eq!(
            fs::read_to_string(dir.join("url")).unwrap(),
            format!("{url}\n")
        );
        assert_eq!(
            fs::read_to_string(dir.join("sha256.digest")).unwrap(),
            format!("{digest}\n")
        );
        assert_eq!(
            fs::read_to_string(dir.join("content-type")).unwrap(),
            "application/octet-stream\n"
        );
        assert!(dir.join(".lock").exists());
        assert!(!dir.join("data.partial").exists());

        // Second fetch is a hit: the source can disappear.
        fs::remove_file(&src).unwrap();
        let mut called = false;
        let p2 = cache
            .fetch(&url, Some(&digest), |_, _| called = true)
            .unwrap();
        assert_eq!(p2, p);
        assert!(!called);
        // Unpinned fetch reuses whatever is cached.
        assert_eq!(cache.fetch(&url, None, |_, _| {}).unwrap(), p);
    }

    #[test]
    fn fetch_rejects_digest_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        fs::write(&src, b"hello").unwrap();
        let cache = Cache::new(tmp.path().join("cache"));
        let url = file_url(&src);
        let wrong = Digest::of_bytes(b"other");
        let err = cache.fetch(&url, Some(&wrong), |_, _| {}).unwrap_err();
        match err {
            Error::DigestMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, wrong);
                assert_eq!(actual, Digest::of_bytes(b"hello"));
            }
            e => panic!("unexpected {e}"),
        }
        let dir = cache.entry_dir(&url);
        assert!(!dir.join("data").exists());
        assert!(!dir.join("data.partial").exists());
        assert!(!dir.join("sha256.digest").exists());
    }

    #[test]
    fn stale_entry_is_replaced_when_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        fs::write(&src, b"v1").unwrap();
        let cache = Cache::new(tmp.path().join("cache"));
        let url = file_url(&src);
        cache.fetch(&url, None, |_, _| {}).unwrap();
        fs::write(&src, b"v2").unwrap();
        // Unpinned: still the old entry.
        let p = cache.fetch(&url, None, |_, _| {}).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"v1");
        // Pinned to v2: re-downloaded.
        let d2 = Digest::of_bytes(b"v2");
        let p = cache.fetch(&url, Some(&d2), |_, _| {}).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"v2");
        assert_eq!(
            fs::read_to_string(cache.entry_dir(&url).join("sha256.digest")).unwrap(),
            format!("{d2}\n")
        );
    }

    #[test]
    fn incomplete_entry_is_not_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        fs::write(&src, b"good").unwrap();
        let cache = Cache::new(tmp.path().join("cache"));
        let url = file_url(&src);
        let dir = cache.entry_dir(&url);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("data"), b"garbage-without-digest").unwrap();
        let p = cache.fetch(&url, None, |_, _| {}).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"good");
    }

    #[test]
    fn bad_urls() {
        let cache = Cache::new("/nonexistent-root-never-created");
        assert!(matches!(
            Source::parse("ftp://x/y"),
            Err(Error::UnsupportedUrl(_))
        ));
        assert!(matches!(
            Source::parse("file://relative/path"),
            Err(Error::UnsupportedUrl(_))
        ));
        assert!(matches!(Source::parse("https://x/y"), Ok(Source::Http(_))));
        assert!(matches!(
            Source::parse("file://localhost/etc/hosts"),
            Ok(Source::File(p)) if p == Path::new("/etc/hosts")
        ));
        let _ = cache;
    }

    #[test]
    fn dir_lock_is_exclusive_across_processes() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = tmp.path().join(".lock");
        let held = DirLock::acquire(&lock).unwrap();
        // A separate process must fail to take the lock non-blockingly.
        let out = Command::new("python3")
            .arg("-I")
            .arg("-c")
            .arg("import fcntl,sys\nf=open(sys.argv[1],'w')\ntry:\n fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\n print('free')\nexcept OSError:\n print('busy')")
            .arg(&lock)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "busy");
        drop(held);
        let out = Command::new("python3")
            .arg("-I")
            .arg("-c")
            .arg("import fcntl,sys\nf=open(sys.argv[1],'w')\nfcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nprint('free')")
            .arg(&lock)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "free");
    }

    fn compress_with(tool: &str, src: &Path, dst: &Path) -> bool {
        let Ok(out) = Command::new(tool).arg("-c").arg(src).output() else {
            return false;
        };
        assert!(out.status.success());
        fs::write(dst, out.stdout).unwrap();
        true
    }

    #[test]
    fn decompress_passthrough_for_plain_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("data");
        fs::write(&p, b"plain bytes").unwrap();
        assert_eq!(decompress_if_needed(&p).unwrap(), p);
        let tiny = tmp.path().join("tiny");
        fs::write(&tiny, b"\x1f").unwrap();
        assert_eq!(decompress_if_needed(&tiny).unwrap(), tiny);
    }

    #[test]
    fn decompress_gzip_and_xz_sparse_and_cached() {
        let tmp = tempfile::tempdir().unwrap();
        // 4 MiB with data only in the middle, so the output should be sparse.
        let mut raw = vec![0u8; 4 << 20];
        raw[1 << 20..(1 << 20) + 100_000]
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = (i % 199) as u8);
        let src = tmp.path().join("img.raw");
        fs::write(&src, &raw).unwrap();

        for tool in ["gzip", "xz"] {
            let dir = tmp.path().join(tool);
            fs::create_dir(&dir).unwrap();
            let comp = dir.join("data");
            if !compress_with(tool, &src, &comp) {
                eprintln!("skipping: {tool} not installed");
                continue;
            }
            let out = decompress_if_needed(&comp).unwrap();
            assert_eq!(out, dir.join("data.raw"));
            assert_eq!(fs::read(&out).unwrap(), raw, "{tool}");
            if cfg!(target_os = "linux") {
                assert!(
                    crate::sparse::allocated_bytes(&out).unwrap() < 1 << 20,
                    "{tool}"
                );
            }
            assert!(!dir.join("data.raw.partial").exists());
            // Cached: a second call does not rewrite the file.
            let mtime = fs::metadata(&out).unwrap().modified().unwrap();
            fs::write(&out, b"sentinel").unwrap();
            assert_eq!(decompress_if_needed(&comp).unwrap(), out);
            assert_eq!(fs::read(&out).unwrap(), b"sentinel");
            let _ = mtime;
        }
    }

    #[test]
    fn decompress_corrupt_input_errors_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("data");
        fs::write(&p, [0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3]).unwrap();
        let err = decompress_if_needed(&p).unwrap_err();
        assert!(matches!(err, Error::Decompress { .. }), "{err}");
        assert!(!tmp.path().join("data.raw").exists());
        assert!(!tmp.path().join("data.raw.partial").exists());
    }
}
