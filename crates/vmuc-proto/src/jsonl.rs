// SPDX-License-Identifier: Apache-2.0
//! JSON lines for the CLI ↔ VMM channel (`vmm.sock`): one object per
//! line, so the socket is debuggable with
//! `echo '{"method":"status"}' | nc -U vmm.sock`.
//!
//! Reads are unbuffered on purpose: after a `vsock_connect` reply the
//! same connection carries raw bytes, and a buffered reader could
//! swallow the first of them.

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Longest line accepted.
pub const MAX_LINE: usize = 1 << 20;

pub fn write_line<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// Reads one line, byte by byte. `Ok(None)` on EOF before any byte.
pub fn read_line<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) if line.is_empty() => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
        if line.len() > MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlRequest, ControlResponse};

    #[test]
    fn one_object_per_line_and_nothing_read_past_it() {
        let mut buf = Vec::new();
        write_line(&mut buf, &ControlRequest::VsockConnect { port: 7700 }).unwrap();
        assert_eq!(
            String::from_utf8(buf.clone()).unwrap(),
            "{\"method\":\"vsock_connect\",\"params\":{\"port\":7700}}\n"
        );
        buf.extend_from_slice(b"RAW");
        let mut cur = io::Cursor::new(buf);
        let req: Option<ControlRequest> = read_line(&mut cur).unwrap();
        assert_eq!(req, Some(ControlRequest::VsockConnect { port: 7700 }));
        let mut rest = Vec::new();
        cur.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"RAW");
    }

    #[test]
    fn eof_and_garbage() {
        let mut empty = io::Cursor::new(Vec::new());
        assert!(
            read_line::<_, ControlResponse>(&mut empty)
                .unwrap()
                .is_none()
        );
        let mut partial = io::Cursor::new(b"{\"result\"".to_vec());
        assert!(read_line::<_, ControlResponse>(&mut partial).is_err());
        let mut junk = io::Cursor::new(b"nope\n".to_vec());
        assert!(read_line::<_, ControlResponse>(&mut junk).is_err());
    }
}
