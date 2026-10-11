// SPDX-License-Identifier: Apache-2.0
//! `vmuc-vmnet-helper --mode shared | --mode bridged --interface NAME`
//!
//! Runs as root through a sudoers rule (`vmuc sudoers`), with the VMM's
//! datagram socket as stdin. See `vmuc_net::helper`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match vmuc_net::helper::Mode::from_args(&args) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("vmuc-vmnet-helper: {e}");
            std::process::exit(2);
        }
    };
    #[cfg(target_os = "macos")]
    if let Err(e) = vmuc_net::helper::run(&mode) {
        eprintln!("vmuc-vmnet-helper: {e}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = mode;
        eprintln!("vmuc-vmnet-helper runs on macOS only");
        std::process::exit(2);
    }
}
