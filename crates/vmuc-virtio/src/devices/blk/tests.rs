// SPDX-License-Identifier: Apache-2.0
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

use super::*;
use crate::device::IrqStatus;
use crate::irq::TestIrq;
use crate::queue::testing::Driver;

struct Rig {
    d: Driver,
    blk: Block,
    file: File,
}

fn rig(size: u64, read_only: bool) -> Rig {
    let mut file = tempfile::tempfile().unwrap();
    file.set_len(size).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"MBR!").unwrap();
    let cfg = BlockConfig {
        read_only,
        serial: "vmuc-test-disk".into(),
    };
    let mut blk = Block::new(file.try_clone().unwrap(), &cfg).unwrap();
    let d = Driver::new(16, true);
    blk.activate(Activation {
        mem: d.mem.clone(),
        queues: vec![d.queue.clone()],
        irq: Arc::new(IrqStatus::new(Arc::new(TestIrq::default()))),
        features: features::RING_EVENT_IDX,
    })
    .unwrap();
    Rig { d, blk, file }
}

impl Rig {
    /// Posts header + data + status, kicks, waits; returns (status, used len).
    fn run(&mut self, kind: u32, sector: u64, data: &[(Gpa, u32, bool)]) -> (u8, u32) {
        let mut hdr = [0u8; 16];
        hdr[0..4].copy_from_slice(&kind.to_le_bytes());
        hdr[8..16].copy_from_slice(&sector.to_le_bytes());
        let h = self.d.buffer(&hdr, 16);
        let st = self.d.buffer(&[0xff], 1);
        let mut chain = vec![(h, 16, false)];
        chain.extend_from_slice(data);
        chain.push((st, 1, true));
        self.d.post(&chain);
        self.blk.doorbell().ring(0);
        let t = Instant::now();
        loop {
            if let Some(&(_, len)) = self.d.used().first() {
                let status: u8 = self.d.mem.read_obj(st).unwrap();
                return (status, len);
            }
            assert!(t.elapsed() < Duration::from_secs(5), "no completion");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

use crate::mem::Gpa;

#[test]
fn config_space_describes_the_disk() {
    let r = rig(8 << 20, false);
    let mut cap = [0u8; 8];
    r.blk.read_config(0, &mut cap);
    assert_eq!(u64::from_le_bytes(cap), (8 << 20) / 512);
    assert_ne!(r.blk.features() & F_DISCARD, 0);
    assert_eq!(r.blk.features() & F_RO, 0);
    let mut align = [0u8; 4];
    r.blk.read_config(44, &mut align);
    assert_eq!(u32::from_le_bytes(align), 8);
}

#[test]
fn writes_and_reads_across_segments() {
    let mut r = rig(1 << 20, false);
    let a = r.d.buffer(&[0xaa; 512], 512);
    let b = r.d.buffer(&[0xbb; 1024], 1024);
    assert_eq!(
        r.run(T_OUT, 3, &[(a, 512, false), (b, 1024, false)]),
        (S_OK, 1)
    );
    let mut disk = vec![0u8; 1536];
    r.file.seek(SeekFrom::Start(3 * 512)).unwrap();
    r.file.read_exact(&mut disk).unwrap();
    assert!(disk[..512].iter().all(|&x| x == 0xaa));
    assert!(disk[512..].iter().all(|&x| x == 0xbb));

    let out1 = r.d.buffer(&[], 1000);
    let out2 = r.d.buffer(&[], 536);
    assert_eq!(
        r.run(T_IN, 3, &[(out1, 1000, true), (out2, 536, true)]),
        (S_OK, 1537)
    );
    let mut got = vec![0u8; 536];
    r.d.mem.read(out2, &mut got).unwrap();
    assert!(got.iter().all(|&x| x == 0xbb));
}

#[test]
fn get_id_flush_and_unsupported() {
    let mut r = rig(1 << 20, false);
    let id = r.d.buffer(&[], 20);
    assert_eq!(r.run(T_GET_ID, 0, &[(id, 20, true)]), (S_OK, 21));
    let mut got = [0u8; 14];
    r.d.mem.read(id, &mut got).unwrap();
    assert_eq!(&got, b"vmuc-test-disk");
    assert_eq!(r.run(T_FLUSH, 0, &[]).0, S_OK);
    assert_eq!(r.run(99, 0, &[]).0, S_UNSUPP);
}

#[test]
fn out_of_range_is_an_io_error() {
    let mut r = rig(1 << 20, false);
    let buf = r.d.buffer(&[], 1024);
    assert_eq!(
        r.run(T_IN, (1 << 20) / 512 - 1, &[(buf, 1024, true)]).0,
        S_IOERR
    );
}

fn discard_seg(sector: u64, count: u32) -> [u8; 16] {
    let mut s = [0u8; 16];
    s[0..8].copy_from_slice(&sector.to_le_bytes());
    s[8..12].copy_from_slice(&count.to_le_bytes());
    s
}

#[test]
fn discard_and_write_zeroes_leave_zeroes_including_unaligned_edges() {
    let mut r = rig(1 << 20, false);
    r.file.seek(SeekFrom::Start(0)).unwrap();
    r.file.write_all(&vec![0x5a; 64 << 10]).unwrap();
    // Sectors 3..=40: unaligned head and tail around an aligned middle.
    let seg = r.d.buffer(&discard_seg(3, 38), 16);
    assert_eq!(r.run(T_DISCARD, 0, &[(seg, 16, false)]).0, S_OK);
    let seg = r.d.buffer(&discard_seg(100, 1), 16);
    assert_eq!(r.run(T_WRITE_ZEROES, 0, &[(seg, 16, false)]).0, S_OK);
    let mut disk = vec![0u8; 64 << 10];
    r.file.seek(SeekFrom::Start(0)).unwrap();
    r.file.read_exact(&mut disk).unwrap();
    for (i, chunk) in disk.chunks(512).enumerate() {
        let zero = (3..=40).contains(&i) || i == 100;
        assert!(
            chunk.iter().all(|&x| x == if zero { 0 } else { 0x5a }),
            "sector {i}"
        );
    }
}

#[test]
fn read_only_disks_refuse_writes() {
    let mut r = rig(1 << 20, true);
    assert_ne!(r.blk.features() & F_RO, 0);
    let buf = r.d.buffer(&[1; 512], 512);
    assert_eq!(r.run(T_OUT, 0, &[(buf, 512, false)]).0, S_IOERR);
    let out = r.d.buffer(&[], 4);
    assert_eq!(r.run(T_IN, 0, &[(out, 4, true)]).0, S_OK);
    let mut got = [0u8; 4];
    r.d.mem.read(out, &mut got).unwrap();
    assert_eq!(&got, b"MBR!");
}
