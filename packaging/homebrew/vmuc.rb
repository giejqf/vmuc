# Homebrew formula for vmuc. Lives in the tap repository as
# Formula/vmuc.rb; this copy is the source of truth. The release workflow
# fills in the version and checksums with packaging/homebrew/render.sh
# and attaches the result to the draft release.
class Vmuc < Formula
  desc "ARM Linux VMs on Apple silicon with host-returnable guest memory"
  homepage "https://github.com/giejqf/vmuc"
  url "https://github.com/giejqf/vmuc/archive/refs/tags/vVERSION.tar.gz"
  sha256 "SHA256_SOURCE"
  license "Apache-2.0"
  head "https://github.com/giejqf/vmuc.git", branch: "main"

  depends_on "rust" => :build
  depends_on arch: :arm64
  depends_on macos: :sequoia

  # Built on Linux by the release workflow (the kernel passes a QEMU boot
  # test there) and pinned here, so an install is reproducible and works
  # offline afterwards. Homebrew's Rust has no musl target, so the static
  # guest agent is a release asset too.
  resource "kernel" do
    url "https://github.com/giejqf/vmuc/releases/download/vVERSION/vmuc-kernel-VERSION-arm64.img"
    sha256 "SHA256_KERNEL"
  end

  resource "agent" do
    url "https://github.com/giejqf/vmuc/releases/download/vVERSION/vmuc-agent-VERSION-aarch64-linux-musl"
    sha256 "SHA256_AGENT"
  end

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/vmuc-cli")
    # vmuc-vmnet-helper: `vmuc sudoers` shows how to copy it somewhere
    # root-owned; sudo never runs this user-writable copy.
    system "cargo", "install", *std_cargo_args(path: "crates/vmuc-net")
    # Hypervisor.framework needs the entitlement; an ad-hoc signature is
    # enough and survives Homebrew's relocation (it re-signs with
    # --preserve-metadata=entitlements).
    system "codesign", "--force", "--sign", "-",
           "--entitlements", "vmuc.entitlements", bin/"vmuc"
    # Paths vmuc looks in: <prefix>/share/vmuc/{kernel/Image,vmuc-agent,templates}.
    resource("kernel").stage do
      (pkgshare/"kernel").install "vmuc-kernel-#{version}-arm64.img" => "Image"
    end
    resource("agent").stage do
      pkgshare.install "vmuc-agent-#{version}-aarch64-linux-musl" => "vmuc-agent"
    end
    chmod 0755, pkgshare/"vmuc-agent"
    (pkgshare/"templates").install Dir["guest/templates/*"]
  end

  def caveats
    <<~EOS
      vmuc needs macOS 15 or newer on Apple silicon. Check the host and
      the install with:
        vmuc info
      Guest networking through vmnet needs a small root helper and a
      sudoers rule; `vmuc sudoers` prints the commands. Without it,
      instances boot without a NIC and ssh, file sharing and port
      forwarding still work.
    EOS
  end

  test do
    assert_match "vmuc #{version}", shell_output("#{bin}/vmuc --version")
    assert_match "com.apple.security.hypervisor",
                 shell_output("codesign -d --entitlements - #{bin}/vmuc 2>&1")
    assert_match "ready:       yes", shell_output("#{bin}/vmuc info")
  end
end
