// SPDX-License-Identifier: Apache-2.0
//! Host side of the guest-agent protocol. Until the VMM exists the only
//! transports are TCP and unix sockets (the QEMU harness forwards the
//! agent's TCP listener); vsock arrives with `ControlRequest::VsockConnect`
//! in M4 and plugs in as another [`AgentConn`].

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context, bail};
use vmuc_proto::frame::{read_frame, write_frame};
use vmuc_proto::{AgentRequest, AgentResponse, PROTO_VERSION};

pub trait AgentConn: Read + Write + Send {
    fn split(&self) -> io::Result<Box<dyn AgentConn>>;
    fn shutdown_write(&self) -> io::Result<()>;
}

impl AgentConn for TcpStream {
    fn split(&self) -> io::Result<Box<dyn AgentConn>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn shutdown_write(&self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

impl AgentConn for UnixStream {
    fn split(&self) -> io::Result<Box<dyn AgentConn>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn shutdown_write(&self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

/// `tcp:HOST:PORT` or `unix:PATH`.
pub fn connect(spec: &str) -> anyhow::Result<Box<dyn AgentConn>> {
    let Some((kind, rest)) = spec.split_once(':') else {
        bail!("agent address must be tcp:HOST:PORT, unix:PATH or vsock:INSTANCE, got {spec}");
    };
    match kind {
        "tcp" => {
            let addr = rest
                .parse()
                .with_context(|| format!("bad tcp address {rest}"))?;
            let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
            s.set_nodelay(true)?;
            Ok(Box::new(s))
        }
        "unix" => Ok(Box::new(UnixStream::connect(rest)?)),
        "vsock" => {
            let home = vmuc_config::VmucHome::detect()?;
            let dir = home.instance_dir(rest)?;
            Ok(Box::new(crate::control::vsock_connect(
                &dir,
                vmuc_proto::AGENT_VSOCK_PORT,
            )?))
        }
        other => bail!("unknown agent transport {other}"),
    }
}

/// One request, one response, on a fresh connection.
pub fn call(spec: &str, req: &AgentRequest) -> anyhow::Result<AgentResponse> {
    let mut c = connect(spec)?;
    write_frame(&mut c, req)?;
    let resp: Option<AgentResponse> = read_frame(&mut c)?;
    resp.ok_or_else(|| anyhow::anyhow!("agent closed the connection without a reply"))
}

/// Parses `METHOD [JSON params]` from the command line into a request:
/// `hello`, `mem_info`, `exec '["uname","-a"]'`, `tunnel 127.0.0.1:22`.
pub fn parse_request(method: &str, params: Option<&str>) -> anyhow::Result<AgentRequest> {
    let v = match (method, params) {
        ("hello", _) => {
            serde_json::json!({"method": "hello", "params": {"proto_version": PROTO_VERSION}})
        }
        ("exec", Some(p)) => {
            let argv: Vec<String> =
                serde_json::from_str(p).context("exec params must be a JSON array")?;
            serde_json::json!({"method": "exec", "params": {"argv": argv, "timeout": 60}})
        }
        ("tunnel", Some(addr)) => serde_json::json!({"method": "tunnel", "params": {"addr": addr}}),
        ("time_sync", _) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            serde_json::json!({"method": "time_sync", "params": {"unix_nanos": now as i64, "max_drift_ms": 1000}})
        }
        (m, Some(p)) => {
            serde_json::json!({"method": m, "params": serde_json::from_str::<serde_json::Value>(p)?})
        }
        (m, None) => serde_json::json!({"method": m}),
    };
    Ok(serde_json::from_value(v)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_and_exec() {
        assert_eq!(
            parse_request("mem_info", None).unwrap(),
            AgentRequest::MemInfo
        );
        assert!(matches!(
            parse_request("exec", Some(r#"["uname","-a"]"#)).unwrap(),
            AgentRequest::Exec { argv, .. } if argv == ["uname", "-a"]
        ));
        assert!(parse_request("nope", None).is_err());
    }
}
