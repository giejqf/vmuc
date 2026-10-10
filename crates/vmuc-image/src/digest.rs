// SPDX-License-Identifier: Apache-2.0
//! Streaming sha256 and the [`Digest`] type.
//!
//! A digest prints in the OCI form `sha256:<64 hex>` and parses from either
//! that form or bare hex. [`DigestWriter`] hashes bytes as they pass through
//! so a download is hashed in the same pass that writes it to disk.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::str::FromStr;

use sha2::{Digest as _, Sha256};

use crate::Error;

/// A sha256 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Wrap raw digest bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lower-case hex without the `sha256:` prefix.
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Digest of an in-memory buffer.
    pub fn of_bytes(data: &[u8]) -> Self {
        Self(Sha256::digest(data).into())
    }

    /// Digest of everything `reader` yields, streamed in 1 MiB chunks.
    pub fn of_reader<R: Read>(mut reader: R) -> io::Result<Self> {
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Self(hasher.finalize().into()))
    }

    /// Digest of a file's contents.
    pub fn of_file(path: &Path) -> io::Result<Self> {
        Self::of_reader(File::open(path)?)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Digest {
    type Err = Error;

    /// Accepts `sha256:<hex>` or `<hex>`; hex may be upper or lower case.
    fn from_str(s: &str) -> Result<Self, Error> {
        let s = s.trim();
        let hex_part = s.strip_prefix("sha256:").unwrap_or(s);
        if hex_part.len() != 64 {
            return Err(Error::InvalidDigest(s.to_owned()));
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(hex_part, &mut out).map_err(|_| Error::InvalidDigest(s.to_owned()))?;
        Ok(Self(out))
    }
}

/// A [`Write`] adapter that hashes everything written through it.
pub struct DigestWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> DigestWriter<W> {
    /// Wrap `inner`.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            written: 0,
        }
    }

    /// Bytes written so far.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Finish hashing and hand back the inner writer.
    pub fn finish(self) -> (W, Digest) {
        (self.inner, Digest(self.hasher.finalize().into()))
    }
}

impl<W: Write> Write for DigestWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Lower-case hex SHA-512 of a file, for images whose publisher only
/// ships a sha512 checksum (Alpine). Streams the file once.
pub fn sha512_hex_of_file(path: &Path) -> io::Result<String> {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha512::new();
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn known_vectors() {
        assert_eq!(Digest::of_bytes(b"").hex(), EMPTY);
        assert_eq!(Digest::of_bytes(b"abc").hex(), ABC);
        assert_eq!(Digest::of_reader(&b"abc"[..]).unwrap().hex(), ABC);
    }

    #[test]
    fn display_and_parse() {
        let d = Digest::of_bytes(b"abc");
        assert_eq!(d.to_string(), format!("sha256:{ABC}"));
        assert_eq!(format!("{d:?}"), format!("sha256:{ABC}"));
        assert_eq!(ABC.parse::<Digest>().unwrap(), d);
        assert_eq!(format!("sha256:{ABC}").parse::<Digest>().unwrap(), d);
        assert_eq!(ABC.to_uppercase().parse::<Digest>().unwrap(), d);
        assert_eq!(format!("  {ABC}\n").parse::<Digest>().unwrap(), d);
        assert!(matches!(
            "sha256:abc".parse::<Digest>(),
            Err(Error::InvalidDigest(_))
        ));
        assert!(matches!(
            format!("md5:{ABC}").parse::<Digest>(),
            Err(Error::InvalidDigest(_))
        ));
        let bad = format!("{}zz", &ABC[..62]);
        assert!(bad.parse::<Digest>().is_err());
    }

    #[test]
    fn of_file_and_writer_agree() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let mut w = DigestWriter::new(File::create(&p).unwrap());
        w.write_all(&data[..1_000_000]).unwrap();
        w.write_all(&data[1_000_000..]).unwrap();
        assert_eq!(w.written(), 3_000_000);
        let (_, d) = w.finish();
        assert_eq!(d, Digest::of_bytes(&data));
        assert_eq!(Digest::of_file(&p).unwrap(), d);
    }
}
