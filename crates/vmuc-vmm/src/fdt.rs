// SPDX-License-Identifier: Apache-2.0
//! The flattened device tree that describes the machine to the guest
//! kernel. Everything in it comes from [`crate::layout`] and the boot
//! parameters; the kernel has no other way to discover hardware.

use std::ops::Range;

use crate::layout::{
    GICD_BASE, GICD_SIZE, GICR_BASE, PL011_BASE, PL011_SPI, PL031_BASE, PL031_SPI, PRIMECELL_SIZE,
    RAM_BASE, VIRTIO_SLOT_SIZE, mpidr, virtio_slot_base, virtio_slot_spi,
};

/// Largest blob the arm64 boot protocol accepts.
pub const FDT_MAX_SIZE: u64 = 2 << 20;

const PHANDLE_GIC: u32 = 1;
const PHANDLE_CLK: u32 = 2;

// Interrupt specifier cells for arm,gic-v3: type, number, flags.
const GIC_SPI: u32 = 0;
const GIC_PPI: u32 = 1;
const IRQ_LEVEL_HIGH: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum FdtError {
    #[error("device tree writer: {0}")]
    Writer(#[from] vm_fdt::Error),
    #[error("device tree is {0} bytes, over the 2 MiB boot protocol limit")]
    TooLarge(usize),
}

/// The architected timer interrupts, as GIC INTIDs.
#[derive(Debug, Clone, Copy)]
pub struct TimerIntids {
    pub secure_phys: u32,
    pub phys: u32,
    pub virt: u32,
    pub hyp: u32,
}

impl Default for TimerIntids {
    /// The architectural assignment, which Hypervisor.framework reports
    /// for the two timers it owns (physical 30, virtual 27).
    fn default() -> Self {
        Self {
            secure_phys: 29,
            phys: 30,
            virt: 27,
            hyp: 26,
        }
    }
}

#[derive(Debug)]
pub struct FdtParams<'a> {
    pub cpus: u32,
    pub ram_size: u64,
    pub cmdline: &'a str,
    /// Guest physical range of the initrd, if one was loaded.
    pub initrd: Option<Range<u64>>,
    /// Bytes of redistributor frames to advertise: one frame pair per vCPU.
    pub gicr_size: u64,
    /// Populated virtio-mmio slots are `0..virtio_slots`.
    pub virtio_slots: u32,
    pub rng_seed: &'a [u8],
    pub kaslr_seed: u64,
    pub timers: TimerIntids,
}

/// Builds the device tree blob.
pub fn build(p: &FdtParams) -> Result<Vec<u8>, FdtError> {
    let mut f = vm_fdt::FdtWriter::new()?;
    let root = f.begin_node("")?;
    f.property_string("compatible", "linux,dummy-virt")?;
    f.property_u32("#address-cells", 2)?;
    f.property_u32("#size-cells", 2)?;
    f.property_u32("interrupt-parent", PHANDLE_GIC)?;
    write_chosen(&mut f, p)?;
    write_cpus(&mut f, p)?;
    write_interrupts(&mut f, p)?;
    write_devices(&mut f, p)?;
    f.end_node(root)?;
    let blob = f.finish()?;
    if blob.len() as u64 > FDT_MAX_SIZE {
        return Err(FdtError::TooLarge(blob.len()));
    }
    Ok(blob)
}

fn uart_path() -> String {
    format!("/pl011@{PL011_BASE:x}")
}

/// `/chosen`, `/aliases` and `/memory`.
fn write_chosen(f: &mut vm_fdt::FdtWriter, p: &FdtParams) -> Result<(), FdtError> {
    let chosen = f.begin_node("chosen")?;
    f.property_string("bootargs", p.cmdline)?;
    f.property_string("stdout-path", &uart_path())?;
    f.property("rng-seed", p.rng_seed)?;
    f.property_u64("kaslr-seed", p.kaslr_seed)?;
    if let Some(initrd) = &p.initrd {
        f.property_u64("linux,initrd-start", initrd.start)?;
        f.property_u64("linux,initrd-end", initrd.end)?;
    }
    f.end_node(chosen)?;

    let aliases = f.begin_node("aliases")?;
    f.property_string("serial0", &uart_path())?;
    f.end_node(aliases)?;

    let memory = f.begin_node(&format!("memory@{RAM_BASE:x}"))?;
    f.property_string("device_type", "memory")?;
    f.property_array_u64("reg", &[RAM_BASE, p.ram_size])?;
    f.end_node(memory)?;
    Ok(())
}

/// `/cpus` and `/psci`.
fn write_cpus(f: &mut vm_fdt::FdtWriter, p: &FdtParams) -> Result<(), FdtError> {
    let cpus = f.begin_node("cpus")?;
    f.property_u32("#address-cells", 1)?;
    f.property_u32("#size-cells", 0)?;
    for i in 0..p.cpus {
        let aff = mpidr(i);
        let cpu = f.begin_node(&format!("cpu@{aff:x}"))?;
        f.property_string("device_type", "cpu")?;
        f.property_string("compatible", "arm,arm-v8")?;
        f.property_string("enable-method", "psci")?;
        f.property_u32("reg", aff as u32)?;
        f.end_node(cpu)?;
    }
    f.end_node(cpus)?;

    let psci = f.begin_node("psci")?;
    f.property_string_list(
        "compatible",
        vec!["arm,psci-1.0".into(), "arm,psci-0.2".into()],
    )?;
    f.property_string("method", "hvc")?;
    f.end_node(psci)?;
    Ok(())
}

/// The GIC and the architected timer.
fn write_interrupts(f: &mut vm_fdt::FdtWriter, p: &FdtParams) -> Result<(), FdtError> {
    let gic = f.begin_node(&format!("intc@{GICD_BASE:x}"))?;
    f.property_string("compatible", "arm,gic-v3")?;
    f.property_u32("#interrupt-cells", 3)?;
    f.property_null("interrupt-controller")?;
    f.property_array_u64("reg", &[GICD_BASE, GICD_SIZE, GICR_BASE, p.gicr_size])?;
    f.property_u32("#redistributor-regions", 1)?;
    f.property_phandle(PHANDLE_GIC)?;
    f.end_node(gic)?;

    let timer = f.begin_node("timer")?;
    f.property_string("compatible", "arm,armv8-timer")?;
    let t = p.timers;
    let mut cells = Vec::with_capacity(12);
    for intid in [t.secure_phys, t.phys, t.virt, t.hyp] {
        cells.extend([GIC_PPI, intid - 16, IRQ_LEVEL_HIGH]);
    }
    f.property_array_u32("interrupts", &cells)?;
    f.property_null("always-on")?;
    f.end_node(timer)?;
    Ok(())
}

/// The clock, PL011, PL031 and the populated virtio-mmio slots.
fn write_devices(f: &mut vm_fdt::FdtWriter, p: &FdtParams) -> Result<(), FdtError> {
    let clk = f.begin_node("apb-pclk")?;
    f.property_string("compatible", "fixed-clock")?;
    f.property_u32("#clock-cells", 0)?;
    f.property_u32("clock-frequency", 24_000_000)?;
    f.property_string("clock-output-names", "clk24mhz")?;
    f.property_phandle(PHANDLE_CLK)?;
    f.end_node(clk)?;

    let uart = f.begin_node(&format!("pl011@{PL011_BASE:x}"))?;
    f.property_string_list(
        "compatible",
        vec!["arm,pl011".into(), "arm,primecell".into()],
    )?;
    f.property_array_u64("reg", &[PL011_BASE, PRIMECELL_SIZE])?;
    f.property_array_u32("interrupts", &[GIC_SPI, PL011_SPI, IRQ_LEVEL_HIGH])?;
    f.property_array_u32("clocks", &[PHANDLE_CLK, PHANDLE_CLK])?;
    f.property_string_list("clock-names", vec!["uartclk".into(), "apb_pclk".into()])?;
    f.end_node(uart)?;

    let rtc = f.begin_node(&format!("pl031@{PL031_BASE:x}"))?;
    f.property_string_list(
        "compatible",
        vec!["arm,pl031".into(), "arm,primecell".into()],
    )?;
    f.property_array_u64("reg", &[PL031_BASE, PRIMECELL_SIZE])?;
    f.property_array_u32("interrupts", &[GIC_SPI, PL031_SPI, IRQ_LEVEL_HIGH])?;
    f.property_u32("clocks", PHANDLE_CLK)?;
    f.property_string("clock-names", "apb_pclk")?;
    f.end_node(rtc)?;

    for slot in 0..p.virtio_slots {
        let base = virtio_slot_base(slot);
        let dev = f.begin_node(&format!("virtio_mmio@{base:x}"))?;
        f.property_string("compatible", "virtio,mmio")?;
        f.property_array_u64("reg", &[base, VIRTIO_SLOT_SIZE])?;
        f.property_array_u32(
            "interrupts",
            &[GIC_SPI, virtio_slot_spi(slot), IRQ_LEVEL_HIGH],
        )?;
        f.property_null("dma-coherent")?;
        f.end_node(dev)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Minimal structure-block walker: `"/path:prop"` → value bytes.
    fn parse(blob: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let be32 = |o: usize| u32::from_be_bytes(blob[o..o + 4].try_into().unwrap());
        assert_eq!(be32(0), 0xd00d_feed);
        let (structs, strings) = (be32(8) as usize, be32(12) as usize);
        let mut out = BTreeMap::new();
        let mut path: Vec<String> = Vec::new();
        let mut o = structs;
        loop {
            let tok = be32(o);
            o += 4;
            match tok {
                1 => {
                    let end = o + blob[o..].iter().position(|&b| b == 0).unwrap();
                    path.push(String::from_utf8(blob[o..end].to_vec()).unwrap());
                    o = (end + 1).next_multiple_of(4);
                }
                2 => {
                    path.pop();
                }
                3 => {
                    let (len, name_off) = (be32(o) as usize, be32(o + 4) as usize);
                    let name_at = strings + name_off;
                    let name_end = name_at + blob[name_at..].iter().position(|&b| b == 0).unwrap();
                    let name = std::str::from_utf8(&blob[name_at..name_end]).unwrap();
                    let p = if path.len() <= 1 {
                        "/".to_owned()
                    } else {
                        format!("/{}", path[1..].join("/"))
                    };
                    out.insert(format!("{p}:{name}"), blob[o + 8..o + 8 + len].to_vec());
                    o = (o + 8 + len).next_multiple_of(4);
                }
                4 => {}
                9 => break,
                t => panic!("bad token {t}"),
            }
        }
        out
    }

    fn cells(v: &[u8]) -> Vec<u32> {
        v.chunks(4)
            .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
            .collect()
    }

    fn params(seed: &[u8]) -> FdtParams<'_> {
        FdtParams {
            cpus: 4,
            ram_size: 1 << 30,
            cmdline: "console=ttyAMA0 panic=-1",
            initrd: Some(0x4800_0000..0x4810_0000),
            gicr_size: 4 * 0x2_0000,
            virtio_slots: 2,
            rng_seed: seed,
            kaslr_seed: 7,
            timers: TimerIntids::default(),
        }
    }

    #[test]
    fn describes_the_machine() {
        let seed = [0xa5u8; 64];
        let t = parse(&build(&params(&seed)).unwrap());
        assert_eq!(t["/chosen:bootargs"], b"console=ttyAMA0 panic=-1\0");
        assert_eq!(t["/chosen:rng-seed"], seed);
        assert_eq!(cells(&t["/chosen:linux,initrd-start"]), [0, 0x4800_0000]);
        assert_eq!(
            cells(&t["/memory@40000000:reg"]),
            [0, RAM_BASE as u32, 0, 1 << 30]
        );
        assert_eq!(cells(&t["/cpus/cpu@3:reg"]), [3]);
        assert!(!t.contains_key("/cpus/cpu@4:reg"));
        assert_eq!(t["/psci:method"], b"hvc\0");
        assert_eq!(
            cells(&t["/intc@8000000:reg"]),
            [0, 0x0800_0000, 0, 0x1_0000, 0, 0x0c00_0000, 0, 0x8_0000]
        );
        assert_eq!(
            cells(&t["/timer:interrupts"]),
            [1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4]
        );
        assert_eq!(cells(&t["/pl011@9000000:interrupts"]), [0, 1, 4]);
        assert_eq!(cells(&t["/virtio_mmio@a001000:interrupts"]), [0, 33, 4]);
        assert!(t.contains_key("/virtio_mmio@a001000:dma-coherent"));
        assert!(!t.contains_key("/virtio_mmio@a002000:reg"));
        assert_eq!(t["/chosen:stdout-path"], b"/pl011@9000000\0");
    }

    #[test]
    fn no_initrd_means_no_initrd_properties() {
        let seed = [0u8; 64];
        let mut p = params(&seed);
        p.initrd = None;
        let t = parse(&build(&p).unwrap());
        assert!(!t.contains_key("/chosen:linux,initrd-start"));
    }
}
