// SPDX-License-Identifier: Apache-2.0
//! What crosses the VMM-helper socketpair besides the hello: the shared
//! region's fd (once, right after the hello, when the helper offers
//! rings) and one-byte kicks (ADR 0001). A VMM that predates the rings
//! sends frames instead, which the helper recognises by their absence of
//! an fd.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// Payload of the datagram that carries the region's fd.
pub const REGION_MARKER: &[u8] = b"vmuc-rings-v1";
/// A kick: "look at the rings".
static KICK: [u8; 1] = *b"k";

/// Sends `region`'s fd with [`REGION_MARKER`].
pub fn send_region(socket: RawFd, region: &OwnedFd) -> io::Result<()> {
    let fd = region.as_raw_fd();
    let mut iov = libc::iovec {
        iov_base: REGION_MARKER.as_ptr().cast_mut().cast(),
        iov_len: REGION_MARKER.len(),
    };
    // Room for one fd, properly aligned.
    let mut cmsg = [0u64; 4];
    // SAFETY: a zeroed msghdr is valid; fields are filled below.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &raw mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg.as_mut_ptr().cast();
    // SAFETY: CMSG_SPACE is a pure size computation.
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) } as _;
    // SAFETY: the control buffer holds one header plus an fd (CMSG_SPACE
    // above, 32 bytes available); the macros stay inside it.
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&raw const msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
        libc::CMSG_DATA(c).cast::<RawFd>().write_unaligned(fd);
    }
    // SAFETY: a valid socket and a message built above.
    if unsafe { libc::sendmsg(socket, &raw const msg, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The first message after the hello.
#[derive(Debug)]
pub enum First {
    /// The VMM's shared region: use the rings.
    Region(OwnedFd),
    /// A frame of this many bytes in the caller's buffer: an older VMM.
    Frame(usize),
    /// Nothing within the socket's receive timeout.
    Idle,
    /// The socket failed or closed.
    Closed,
}

/// Receives one datagram into `buf`, taking an fd if one came with it.
pub fn recv_first(socket: RawFd, buf: &mut [u8]) -> First {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let mut cmsg = [0u64; 8];
    // SAFETY: a zeroed msghdr is valid; fields are filled below.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &raw mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg.as_mut_ptr().cast();
    msg.msg_controllen = size_of_val(&cmsg) as _;
    // SAFETY: a valid socket and message.
    let n = unsafe { libc::recvmsg(socket, &raw mut msg, 0) };
    if n < 0 {
        return match io::Error::last_os_error().raw_os_error() {
            Some(libc::EAGAIN | libc::EINTR) => First::Idle,
            _ => First::Closed,
        };
    }
    let mut fd = None;
    // SAFETY: walking the control messages the kernel wrote into `cmsg`.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&raw const msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let raw = libc::CMSG_DATA(c).cast::<RawFd>().read_unaligned();
                // Owning it closes it whatever happens next.
                let owned = OwnedFd::from_raw_fd(raw);
                fd.get_or_insert(owned);
            }
            c = libc::CMSG_NXTHDR(&raw const msg, c);
        }
    }
    match fd {
        Some(fd) if &buf[..n as usize] == REGION_MARKER => First::Region(fd),
        Some(_) => First::Closed, // an fd with anything else: refuse
        None => First::Frame(n as usize),
    }
}

/// Tells the other side to look at the rings. A full socket already holds
/// a kick, so failures are ignored.
pub fn kick(socket: RawFd) {
    // SAFETY: sending one byte from a static.
    unsafe { libc::send(socket, KICK.as_ptr().cast(), 1, libc::MSG_DONTWAIT) };
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixDatagram;

    use super::*;
    use crate::region::Region;

    #[test]
    fn the_region_crosses_and_frames_and_kicks_are_told_apart() {
        let (vmm, helper) = UnixDatagram::pair().unwrap();
        let (region, fd) = Region::create(1 << 16).unwrap();
        send_region(vmm.as_raw_fd(), &fd).unwrap();
        let mut buf = [0u8; 64];
        let First::Region(got) = recv_first(helper.as_raw_fd(), &mut buf) else {
            panic!("no region");
        };
        let mapped = Region::from_fd(&got, 1 << 16).unwrap();
        // SAFETY: in-bounds byte accesses of two mappings of one object.
        unsafe {
            region.base().write_volatile(7);
            assert_eq!(mapped.base().read_volatile(), 7);
        }

        vmm.send(b"a frame").unwrap();
        assert!(matches!(
            recv_first(helper.as_raw_fd(), &mut buf),
            First::Frame(7)
        ));
        kick(vmm.as_raw_fd());
        assert!(matches!(
            recv_first(helper.as_raw_fd(), &mut buf),
            First::Frame(1)
        ));

        helper
            .set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .unwrap();
        assert!(matches!(
            recv_first(helper.as_raw_fd(), &mut buf),
            First::Idle
        ));
    }
}
