// SPDX-License-Identifier: Apache-2.0
//! Listening sockets: AF_VSOCK (production), TCP and unix (harness).

use std::io::{self, Read, Write};
use std::mem;
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};

use anyhow::{Context, bail};

/// A connected peer. Boxed trait object so the serving loop is agnostic.
pub trait Conn: Read + Write + Send {
    /// A second handle to the same socket for the tunnel's second
    /// direction. Both halves share the fd; dropping one closes it only
    /// when the last handle goes.
    fn try_clone_conn(&self) -> io::Result<Box<dyn Conn>>;
    fn shutdown_write(&self) -> io::Result<()>;
}

impl Conn for TcpStream {
    fn try_clone_conn(&self) -> io::Result<Box<dyn Conn>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn shutdown_write(&self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

impl Conn for UnixStream {
    fn try_clone_conn(&self) -> io::Result<Box<dyn Conn>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn shutdown_write(&self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

/// An accepted AF_VSOCK stream. The standard library has no vsock type,
/// so this wraps the raw fd.
pub struct VsockStream(OwnedFd);

impl Read for VsockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `buf` is a valid writable slice of `buf.len()` bytes.
        let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }
}

impl Write for VsockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: `buf` is a valid readable slice of `buf.len()` bytes.
        let n = unsafe { libc::write(self.0.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Conn for VsockStream {
    fn try_clone_conn(&self) -> io::Result<Box<dyn Conn>> {
        Ok(Box::new(VsockStream(self.0.try_clone()?)))
    }
    fn shutdown_write(&self) -> io::Result<()> {
        // SAFETY: valid fd.
        if unsafe { libc::shutdown(self.0.as_raw_fd(), libc::SHUT_WR) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

pub enum Listener {
    Vsock(OwnedFd),
    Tcp(TcpListener),
    Unix(UnixListener),
}

impl Listener {
    /// `vsock:PORT`, `tcp:ADDR:PORT` or `unix:PATH`.
    pub fn bind(spec: &str) -> anyhow::Result<Self> {
        let (kind, rest) = spec
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("listen spec needs kind:value, got {spec}"))?;
        match kind {
            "vsock" => {
                let port: u32 = rest.parse().context("vsock port")?;
                Ok(Self::Vsock(bind_vsock(port)?))
            }
            "tcp" => Ok(Self::Tcp(TcpListener::bind(rest).context("tcp bind")?)),
            "unix" => {
                let _ = std::fs::remove_file(rest);
                Ok(Self::Unix(UnixListener::bind(rest).context("unix bind")?))
            }
            other => bail!("unknown listener kind {other}"),
        }
    }

    /// Blocks for the next connection. Vsock connections from a CID
    /// other than the host are dropped and the loop continues.
    pub fn accept(&self) -> io::Result<Box<dyn Conn>> {
        match self {
            Self::Tcp(l) => Ok(Box::new(l.accept()?.0)),
            Self::Unix(l) => Ok(Box::new(l.accept()?.0)),
            Self::Vsock(fd) => loop {
                let (stream, cid) = accept_vsock(fd.as_raw_fd())?;
                if cid == vmuc_proto::HOST_CID {
                    return Ok(Box::new(stream));
                }
                eprintln!("vmuc-agent: rejecting vsock connection from CID {cid}");
            },
        }
    }
}

fn bind_vsock(port: u32) -> anyhow::Result<OwnedFd> {
    // SAFETY: plain socket() call.
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("socket(AF_VSOCK)");
    }
    // SAFETY: `fd` is a fresh, owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: zeroed sockaddr_vm is a valid starting point.
    let mut addr: libc::sockaddr_vm = unsafe { mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_cid = libc::VMADDR_CID_ANY;
    addr.svm_port = port;
    // SAFETY: `addr` is a valid sockaddr_vm of the stated length.
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&raw const addr).cast(),
            mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error()).context("bind(AF_VSOCK)");
    }
    // SAFETY: valid fd.
    if unsafe { libc::listen(fd.as_raw_fd(), 64) } < 0 {
        return Err(io::Error::last_os_error()).context("listen(AF_VSOCK)");
    }
    Ok(fd)
}

fn accept_vsock(listener: RawFd) -> io::Result<(VsockStream, u32)> {
    // SAFETY: zeroed sockaddr_vm is valid; accept4 fills it.
    let mut peer: libc::sockaddr_vm = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
    // SAFETY: `peer`/`len` are valid out-parameters.
    let fd = unsafe {
        libc::accept4(
            listener,
            (&raw mut peer).cast(),
            &raw mut len,
            libc::SOCK_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh, owned descriptor.
    Ok((
        VsockStream(unsafe { OwnedFd::from_raw_fd(fd) }),
        peer.svm_cid,
    ))
}
