# Configuration reference

Generated from the schema (`crates/vmuc-config/src/reference.rs`); the
commented YAML version is `guest/templates/reference.yaml`. A setting
left out falls back to the next layer: built-in default <
`~/.vmuc/_config/default.yaml` < the instance's `vmuc.yaml` <
`~/.vmuc/_config/override.yaml`. `vmuc create --set path=value` and
`vmuc edit` change an instance's `vmuc.yaml`.

## Basic configuration

| Key | Default | Meaning |
|---|---|---|
| `base` | [] (and `default.yaml` uses the Ubuntu image and the ~ mount) | Templates to flatten into this one at `vmuc create` time. Each entry is `template:NAME` (searched in $VMUC_TEMPLATES_PATH, ~/.vmuc/_templates, the installed templates, then the built-in ones) or a file path relative to this file. The child wins for scalars and maps; `provision` and `probes` run base-first; other lists are child-first; mounts merge by location; a non-empty child `images` replaces the base's. Earlier entries win over later ones. Nesting is limited to 8 levels. |
| `images` | [] (an instance cannot start without an image) | Root filesystem candidates, tried in order: the first one that downloads and verifies is used. |
| `image` |  | Another name for `images`. |
| `images[].location` | required | An https:// URL or a local path; qcow2 and raw images, optionally compressed. |
| `images[].arch` | "aarch64" | Always "aarch64" ("arm64" is accepted). |
| `images[].digest` | none | "sha256:..." or "sha512:..."; strongly recommended. |
| `images[].kernel` | the kernel vmuc ships | A kernel to boot instead of vmuc's. |
| `images[].kernel.location` | required | An https:// URL or a local path to an arm64 `Image`. |
| `images[].kernel.digest` | none | "sha256:..." or "sha512:...". |
| `images[].kernel.cmdline` | vmuc's command line | The kernel command line. |
| `cpus` | 4 | Number of vCPUs. |
| `memory` | min("8GiB", half of the host's RAM) | Guest RAM ceiling. The host pays only for pages the guest touches and gets back what the guest frees (free page reporting is always on). Binary units: "8GiB", "8G", "8192MiB", "8192M". Minimum 64MiB. |
| `disk` | "100GiB" | Virtual disk size: a sparse raw file that grows on demand. Minimum 1GiB. |
| `mounts` | the host home ("~") read-only at the same path | Host directories shared into the guest over virtiofs. macOS asks once before the guest may read Desktop, Documents or Downloads. |
| `mounts[].location` | required | Host path; "~" expands to the host home. |
| `mounts[].mountPoint` | the expanded host path (mirrored) | Guest path. |
| `mounts[].writable` | false | Whether the guest may write. |

## Advanced configuration

| Key | Default | Meaning |
|---|---|---|
| `ssh` |  | How `vmuc shell` and plain ssh reach the guest (always through vsock). |
| `ssh.loadDotSSHPubKeys` | false | Also authorize ~/.ssh/*.pub besides ~/.vmuc/_config/user.pub. |
| `ssh.forwardAgent` | false | Forward the host ssh agent into `vmuc shell`. |
| `network` |  | Guest networking through macOS vmnet, via the root helper `vmuc sudoers` installs; without it the instance boots without a NIC (ssh, shares and port forwards still work over vsock). |
| `network.mode` | "shared" | "shared": NAT behind the host, the guest gets a 192.168.x address the host can reach. "bridged": the guest joins the network of `interface`. |
| `network.interface` | none | Host interface for bridged mode (e.g. "en0"); required there. |
| `network.macAddress` | a stable address derived from the instance name | The NIC's MAC address. |
| `portForwards` | [] (plus the catch-all rule) | Guest TCP listeners appear on the host, matched against these rules first to last; the override, instance and default lists are concatenated in that order and a catch-all rule (every guest listener on 0.0.0.0 or 127.0.0.1, ports 1024 and up except 22, to the same port on 127.0.0.1) ends the list. Forwarding rides vsock and needs no NIC. |
| `portForwards[].guestIp` | 127.0.0.1 (also matches 0.0.0.0) | Guest address the rule matches. |
| `portForwards[].guestPort` | none | Guest port; exclusive with guestPortRange. |
| `portForwards[].guestPortRange` | [1, 65535] | Guest ports the rule matches. |
| `portForwards[].hostIp` | 127.0.0.1 | Host address to listen on. |
| `portForwards[].hostPort` | none | Host port; exclusive with hostPortRange. |
| `portForwards[].hostPortRange` | the guest range | Host ports, mapped one to one. |
| `portForwards[].proto` | any | tcp \| udp \| any. Only TCP is forwarded today. |
| `portForwards[].ignore` | false | Match and do not forward. |
| `portForwards[].static` | false | Forward even while nothing listens in the guest. |
| `provision` | [] | Scripts run on every boot from the seed after the agent is up, in list order: `boot` first (cloud-init bootcmd, early), then `data` files are written, then the network is waited for (when there is a NIC), then `system` scripts run as root and `user` scripts as the instance user. Scripts see VMUC_* variables (VMUC_USER, VMUC_UID, VMUC_HOME, VMUC_BOOT_ID, VMUC_PARAM_<KEY>, ...). Make them idempotent. |
| `provision[].mode` | system | system \| user \| boot \| data. |
| `provision[].script` | none | The script (system, user and boot modes). |
| `provision[].path` | none | Where a data entry writes `content`. |
| `provision[].content` | none | What a data entry writes. |
| `provision[].owner` | "root:root" | Owner of a data file. |
| `provision[].permissions` | "0644" | Mode of a data file. |
| `provision[].overwrite` | true | Whether a data entry replaces an existing file. |
| `probes` | [] | Readiness probes the host runs through the agent until they succeed (timeout: `vmuc start --timeout`). |
| `probes[].mode` | readiness | The only mode. |
| `probes[].description` | none | Shown while waiting. |
| `probes[].script` | required | Succeeds (exit 0) when ready. |
| `probes[].hint` | none | Printed when the probe fails. |
| `env` | {} | Environment variables for provisioning scripts and the login shell. |
| `param` | {} | Template parameters, exported to scripts as VMUC_PARAM_<KEY>. Keys match [A-Za-z_][A-Za-z0-9_]*. |
| `user` |  | The guest account `vmuc shell` logs in as. |
| `user.name` | the host user name when it is a valid POSIX name that is not a system account, else "vmuc" | Login name. |
| `user.uid` | the host uid when 500 <= uid < 60000, else 1000 | Numeric user ID (matching the host's keeps shared files' owners right). |
| `user.home` | /home/<name>.linux (clear of the mirrored host home) | Home directory. |
| `user.shell` | /bin/bash | Login shell. |
| `user.comment` | "" | GECOS field. |
| `user.passwordlessSudo` | true | Whether sudo asks for a password. |
| `timezone` | the host's zone, else "UTC" | IANA zone name applied inside the guest. |
| `memoryPolicy` |  | How vmuc takes memory back beyond free page reporting, which is always on. |
| `memoryPolicy.pressureBalloon` | false | Inflate the balloon when macOS reports memory pressure (hysteresis rules in docs/plan.md). |
| `memoryPolicy.minAvailable` | "256MiB" | The balloon never pushes the guest's available memory below this. |
| `message` | "" | Printed after `vmuc start`. |
| `dns` | [] (the DNS server vmnet's DHCP hands out: the host's resolver) | Nameservers written to the guest's resolv.conf instead. |
| `hostResolver` | true | Reserved for vmuc's own user-space network (v0.2); ignored with vmnet, whose DHCP already points the guest at the host's resolver. |
