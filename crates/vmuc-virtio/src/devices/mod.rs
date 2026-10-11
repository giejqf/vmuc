// SPDX-License-Identifier: Apache-2.0
//! Device models, and a byte-stream view of descriptor chains they share.

pub mod blk;

pub mod rng;

use crate::mem::{Gpa, GuestMemory, MemResult};
use crate::queue::DescChain;

/// The readable or writable buffers of a chain, as one byte stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segments(pub Vec<(Gpa, u64)>);

impl Segments {
    pub fn readable(chain: &DescChain) -> Self {
        Self(
            chain
                .readable()
                .map(|d| (d.addr, u64::from(d.len)))
                .collect(),
        )
    }

    pub fn writable(chain: &DescChain) -> Self {
        Self(
            chain
                .writable()
                .map(|d| (d.addr, u64::from(d.len)))
                .collect(),
        )
    }

    pub fn len(&self) -> u64 {
        self.0.iter().map(|s| s.1).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Splits after the first `n` bytes (fewer if the stream is shorter).
    pub fn split_at(&self, mut n: u64) -> (Self, Self) {
        let (mut head, mut tail) = (Vec::new(), Vec::new());
        for &(addr, len) in &self.0 {
            if n == 0 {
                tail.push((addr, len));
            } else if len <= n {
                head.push((addr, len));
                n -= len;
            } else {
                head.push((addr, n));
                tail.push((addr + n, len - n));
                n = 0;
            }
        }
        (Self(head), Self(tail))
    }

    /// Copies the stream's first `buf.len()` bytes into `buf`; returns how
    /// many there were.
    pub fn read_into(&self, mem: &GuestMemory, buf: &mut [u8]) -> MemResult<usize> {
        let mut done = 0;
        for &(addr, len) in &self.0 {
            if done == buf.len() {
                break;
            }
            let n = (len as usize).min(buf.len() - done);
            mem.read(addr, &mut buf[done..done + n])?;
            done += n;
        }
        Ok(done)
    }

    /// Copies `data` into the stream; returns how much fit.
    pub fn write_from(&self, mem: &GuestMemory, data: &[u8]) -> MemResult<usize> {
        let mut done = 0;
        for &(addr, len) in &self.0 {
            if done == data.len() {
                break;
            }
            let n = (len as usize).min(data.len() - done);
            mem.write(addr, &data[done..done + n])?;
            done += n;
        }
        Ok(done)
    }

    /// Host `iovec`s for zero-copy I/O straight into guest buffers.
    pub fn iovecs(&self, mem: &GuestMemory) -> MemResult<Vec<libc::iovec>> {
        self.0
            .iter()
            .map(|&(addr, len)| {
                Ok(libc::iovec {
                    iov_base: mem.host_ptr(addr, len)?.cast(),
                    iov_len: len as usize,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_copy_across_segments() {
        let mem = GuestMemory::from_vec(0, vec![0; 256]);
        let s = Segments(vec![(0, 4), (100, 6), (200, 10)]);
        assert_eq!(s.len(), 20);
        let (h, t) = s.split_at(7);
        assert_eq!(h, Segments(vec![(0, 4), (100, 3)]));
        assert_eq!(t, Segments(vec![(103, 3), (200, 10)]));
        assert_eq!(s.write_from(&mem, b"0123456789abcdef").unwrap(), 16);
        let mut b = [0u8; 8];
        assert_eq!(t.read_into(&mem, &mut b).unwrap(), 8);
        assert_eq!(&b, b"789abcde");
        let (all, none) = s.split_at(99);
        assert_eq!((all.len(), none.len()), (20, 0));
        assert_eq!(s.iovecs(&mem).unwrap().len(), 3);
    }
}
