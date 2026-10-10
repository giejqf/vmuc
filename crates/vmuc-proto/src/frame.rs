// SPDX-License-Identifier: Apache-2.0
//! Length-prefixed JSON frames: a big-endian `u32` byte count followed by
//! that many bytes of JSON. Works over any `Read`/`Write` (unix socket,
//! vsock, TCP in tests).

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Largest frame accepted; control messages are tiny, `Exec` output is
/// the only thing that can grow.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large",
        ));
    }
    let len = (body.len() as u32).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(&body)?;
    w.flush()
}

/// Reads one frame. `Ok(None)` on a clean EOF before any byte of the
/// length prefix.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentRequest, AgentResponse};

    #[test]
    fn round_trip_two_frames() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &AgentRequest::MemInfo).unwrap();
        write_frame(&mut buf, &AgentResponse::Ok).unwrap();
        let mut cur = io::Cursor::new(buf);
        let a: Option<AgentRequest> = read_frame(&mut cur).unwrap();
        assert_eq!(a, Some(AgentRequest::MemInfo));
        let b: Option<AgentResponse> = read_frame(&mut cur).unwrap();
        assert_eq!(b, Some(AgentResponse::Ok));
        let c: Option<AgentResponse> = read_frame(&mut cur).unwrap();
        assert_eq!(c, None);
    }

    #[test]
    fn truncated_body_is_an_error_not_eof() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &AgentRequest::MemInfo).unwrap();
        buf.truncate(buf.len() - 2);
        let mut cur = io::Cursor::new(buf);
        assert!(read_frame::<_, AgentRequest>(&mut cur).is_err());
    }

    #[test]
    fn oversized_prefix_rejected() {
        let mut cur = io::Cursor::new(u32::MAX.to_be_bytes().to_vec());
        assert!(read_frame::<_, AgentRequest>(&mut cur).is_err());
    }
}
