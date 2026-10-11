// SPDX-License-Identifier: Apache-2.0
//! virtio-blk over a raw (sparse) file.
//!
//! Reads and writes go straight between the file and guest buffers with
//! `preadv`/`pwritev`, no bounce copy. FLUSH is `fsync` (data reaches the
//! drive, not `F_FULLFSYNC`'s cache flush; a dev VM trades power-loss
//! durability for journal-commit latency). DISCARD and WRITE_ZEROES punch
//! holes, so space the guest frees goes back to APFS; the unaligned edges
//! of a range are written as zeroes because `F_PUNCHHOLE` needs 4 KiB
//! alignment.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::Arc;

use vmuc_image::SparseFileExt;

use crate::bus::put_le;
use crate::device::{
    Activation, Doorbell, QueueHandler, VirtioDevice, Worker, WorkerCtx, device_type, features,
};
use crate::devices::Segments;
use crate::mem::GuestMemory;
use crate::queue::DescChain;

const F_SEG_MAX: u64 = 1 << 2;
const F_RO: u64 = 1 << 5;
const F_BLK_SIZE: u64 = 1 << 6;
const F_FLUSH: u64 = 1 << 9;
const F_TOPOLOGY: u64 = 1 << 10;
const F_DISCARD: u64 = 1 << 13;
const F_WRITE_ZEROES: u64 = 1 << 14;

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const T_GET_ID: u32 = 8;
const T_DISCARD: u32 = 11;
const T_WRITE_ZEROES: u32 = 13;

const S_OK: u8 = 0;
const S_IOERR: u8 = 1;
const S_UNSUPP: u8 = 2;

const SECTOR: u64 = 512;
/// Hole-punching granularity (the APFS block size).
const PUNCH_ALIGN: u64 = 4096;
const QUEUE_SIZE: u16 = 256;
const ID_LEN: usize = 20;
/// Per-segment discard/write-zeroes limit, in sectors (1 GiB).
const MAX_DISCARD_SECTORS: u32 = 1 << 21;

/// Options for one disk.
#[derive(Debug, Clone)]
pub struct BlockConfig {
    pub read_only: bool,
    /// Reported by GET_ID (`/sys/block/vdX/serial`), at most 20 bytes.
    pub serial: String,
}

struct Handler {
    file: File,
    capacity: u64,
    read_only: bool,
    id: [u8; ID_LEN],
    zeroes: Vec<u8>,
}

impl Handler {
    /// Executes one request; returns the status and the bytes written
    /// into the guest's buffers (data, not counting the status byte).
    fn request(&mut self, mem: &GuestMemory, chain: &DescChain) -> (u8, Option<Segments>, u32) {
        let (hdr_segs, out_data) = Segments::readable(chain).split_at(16);
        let writable = Segments::writable(chain);
        if writable.is_empty() {
            return (S_IOERR, None, 0);
        }
        let (in_data, status) = writable.split_at(writable.len() - 1);
        let mut hdr = [0u8; 16];
        if hdr_segs.read_into(mem, &mut hdr).unwrap_or(0) != 16 {
            return (S_IOERR, Some(status), 0);
        }
        let kind = u32::from_le_bytes(hdr[0..4].try_into().unwrap_or_default());
        let sector = u64::from_le_bytes(hdr[8..16].try_into().unwrap_or_default());
        let writes = matches!(kind, T_OUT | T_DISCARD | T_WRITE_ZEROES);
        if writes && self.read_only {
            return (S_IOERR, Some(status), 0);
        }
        let result = match kind {
            T_IN => self
                .transfer(mem, &in_data, sector, false)
                .map(|()| in_data.len() as u32),
            T_OUT => self.transfer(mem, &out_data, sector, true).map(|()| 0),
            // SAFETY: plain syscall on our own descriptor.
            T_FLUSH => io_result(unsafe { libc::fsync(self.file.as_raw_fd()) }).map(|()| 0),
            T_GET_ID => in_data
                .write_from(mem, &self.id)
                .map(|n| n as u32)
                .map_err(io::Error::other),
            T_DISCARD | T_WRITE_ZEROES => self.discard(mem, &out_data).map(|()| 0),
            _ => return (S_UNSUPP, Some(status), 0),
        };
        match result {
            Ok(n) => (S_OK, Some(status), n),
            Err(e) => {
                tracing::warn!(kind, sector, "virtio-blk: {e}");
                (S_IOERR, Some(status), 0)
            }
        }
    }

    fn check_range(&self, sector: u64, len: u64) -> io::Result<u64> {
        let off = sector
            .checked_mul(SECTOR)
            .ok_or(io::ErrorKind::InvalidInput)?;
        match off.checked_add(len) {
            Some(end) if end <= self.capacity => Ok(off),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "request beyond end of disk",
            )),
        }
    }

    /// `preadv`/`pwritev` the whole of `segs` at `sector`, resuming after
    /// short transfers; a read past end of file is zero-filled.
    fn transfer(
        &self,
        mem: &GuestMemory,
        segs: &Segments,
        sector: u64,
        write: bool,
    ) -> io::Result<()> {
        let mut off = self.check_range(sector, segs.len())?;
        let mut iov = segs.iovecs(mem).map_err(io::Error::other)?;
        let mut at = 0;
        while at < iov.len() {
            let fd = self.file.as_raw_fd();
            let cnt = (iov.len() - at).min(1024) as libc::c_int;
            // SAFETY: every iovec points into guest RAM validated by
            // `iovecs`; the file descriptor is ours.
            let n = unsafe {
                if write {
                    libc::pwritev(fd, iov[at..].as_ptr(), cnt, off as libc::off_t)
                } else {
                    libc::preadv(fd, iov[at..].as_ptr(), cnt, off as libc::off_t)
                }
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n == 0 {
                if write {
                    return Err(io::ErrorKind::WriteZero.into());
                }
                // Past end of file: the rest reads as zeroes.
                for v in &iov[at..] {
                    // SAFETY: as above.
                    unsafe { std::ptr::write_bytes(v.iov_base.cast::<u8>(), 0, v.iov_len) };
                }
                return Ok(());
            }
            off += n as u64;
            let mut n = n as usize;
            while at < iov.len() && n >= iov[at].iov_len {
                n -= iov[at].iov_len;
                at += 1;
            }
            if n > 0 {
                // SAFETY: advancing within the same guest buffer.
                iov[at].iov_base = unsafe { iov[at].iov_base.cast::<u8>().add(n).cast() };
                iov[at].iov_len -= n;
            }
        }
        Ok(())
    }

    /// DISCARD and WRITE_ZEROES both end as zeroes on disk: holes for the
    /// aligned middle, written zeroes for the edges.
    fn discard(&mut self, mem: &GuestMemory, segs: &Segments) -> io::Result<()> {
        let mut raw = vec![0u8; segs.len().min(16 * 256) as usize];
        segs.read_into(mem, &mut raw).map_err(io::Error::other)?;
        for seg in raw.as_chunks::<16>().0 {
            let sector = u64::from_le_bytes(seg[0..8].try_into().unwrap_or_default());
            let count = u32::from_le_bytes(seg[8..12].try_into().unwrap_or_default());
            if count > MAX_DISCARD_SECTORS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "discard too large",
                ));
            }
            let len = u64::from(count) * SECTOR;
            let start = self.check_range(sector, len)?;
            let end = start + len;
            let (a, b) = (
                start.next_multiple_of(PUNCH_ALIGN),
                end / PUNCH_ALIGN * PUNCH_ALIGN,
            );
            if a < b {
                self.zero(start, a - start)?;
                self.file.punch_hole(a, b - a)?;
                self.zero(b, end - b)?;
            } else {
                self.zero(start, len)?;
            }
        }
        Ok(())
    }

    fn zero(&mut self, off: u64, len: u64) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        if len == 0 {
            return Ok(());
        }
        self.zeroes.resize(len as usize, 0);
        self.file.write_all_at(&self.zeroes, off)
    }
}

fn io_result(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

impl QueueHandler for Handler {
    fn process(&mut self, ctx: &mut WorkerCtx, _bits: u32) {
        let (mem, q) = (&ctx.mem, &mut ctx.queues[0]);
        loop {
            while let Ok(Some(chain)) = q.pop(mem) {
                let (st, status, data) = self.request(mem, &chain);
                let len = match status {
                    Some(s) if s.write_from(mem, &[st]).is_ok() => data + 1,
                    _ => 0,
                };
                let _ = q.add_used(mem, chain.head, len);
            }
            let _ = q.publish_used(mem);
            if !q.enable_notification(mem).unwrap_or(false) {
                break;
            }
        }
        if q.needs_interrupt(mem).unwrap_or(true) {
            ctx.irq.signal_used();
        }
    }
}

pub struct Block {
    config: [u8; 60],
    features: u64,
    worker: Worker,
}

impl Block {
    pub fn new(file: File, cfg: &BlockConfig) -> io::Result<Self> {
        let capacity = file.metadata()?.len() / SECTOR * SECTOR;
        let mut config = [0u8; 60];
        let mut put =
            |off: usize, bytes: &[u8]| config[off..off + bytes.len()].copy_from_slice(bytes);
        put(0, &(capacity / SECTOR).to_le_bytes());
        put(12, &u32::from(QUEUE_SIZE - 2).to_le_bytes()); // seg_max
        put(20, &(SECTOR as u32).to_le_bytes()); // blk_size
        put(24, &[3]); // physical block 2^3 sectors (4 KiB)
        put(26, &8u16.to_le_bytes()); // min_io_size, sectors
        put(34, &1u16.to_le_bytes()); // num_queues
        put(36, &MAX_DISCARD_SECTORS.to_le_bytes());
        put(40, &1u32.to_le_bytes()); // max_discard_seg
        put(44, &((PUNCH_ALIGN / SECTOR) as u32).to_le_bytes());
        put(48, &MAX_DISCARD_SECTORS.to_le_bytes());
        put(52, &1u32.to_le_bytes()); // max_write_zeroes_seg
        put(56, &[1]); // write_zeroes_may_unmap
        let mut features = features::RING_EVENT_IDX
            | features::RING_INDIRECT_DESC
            | F_SEG_MAX
            | F_BLK_SIZE
            | F_FLUSH
            | F_TOPOLOGY;
        if cfg.read_only {
            features |= F_RO;
        } else {
            features |= F_DISCARD | F_WRITE_ZEROES;
        }
        let mut id = [0u8; ID_LEN];
        let n = cfg.serial.len().min(ID_LEN);
        id[..n].copy_from_slice(&cfg.serial.as_bytes()[..n]);
        let handler = Handler {
            file,
            capacity,
            read_only: cfg.read_only,
            id,
            zeroes: Vec::new(),
        };
        Ok(Self {
            config,
            features,
            worker: Worker::spawn("virtio-blk", handler)?,
        })
    }
}

impl VirtioDevice for Block {
    fn device_type(&self) -> u32 {
        device_type::BLOCK
    }
    fn features(&self) -> u64 {
        self.features
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![QUEUE_SIZE]
    }
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let off = offset as usize;
        if off + data.len() <= self.config.len() {
            data.copy_from_slice(&self.config[off..off + data.len()]);
        } else {
            put_le(data, 0);
        }
    }
    fn activate(&mut self, act: Activation) -> Result<(), String> {
        self.worker.activate(act)
    }
    fn reset(&mut self) {
        self.worker.reset();
    }
    fn doorbell(&self) -> Arc<dyn Doorbell> {
        self.worker.doorbell()
    }
}

#[cfg(test)]
mod tests;
