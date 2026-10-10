# Homebrew formula for vmuc. Lives in the tap repository as
# Formula/vmuc.rb; this copy is the source of truth and is mirrored on
# release. Placeholders (VERSION, SHA256) are filled by the release job.
class Vmuc < Formula
  desc "ARM Linux VMs on Apple silicon with host-returnable guest memory"
  homepage "https://github.com/giejqf/vmuc"
  url "https://github.com/giejqf/vmuc/archive/refs/tags/vVERSION.tar.gz"
  sha256 "SHA256_SOURCE"
  license "Apache-2.0"
  head "https://github.com/giejqf/vmuc.git", branch: "main"

  depends_on arch: :arm64
  depends_on macos: :sequoia
  depends_on "rust" => :build

  # The guest kernel is built on Linux by the release workflow and pinned
  # here so an install is reproducible and works offline afterwards.
  resource "kernel" do
    url "https://github.com/giejqf/vmuc/releases/download/vVERSION/vmuc-kernel-VERSION-arm64.img"
    sha256 "SHA256_KERNEL"
  end

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/vmuc-cli")
    # Hypervisor.framework needs the entitlement; an ad-hoc signature is
    # enough and survives Homebrew's bottle relocation (it re-signs with
    # --preserve-metadata=entitlements).
    system "codesign", "--force", "--sign", "-",
           "--entitlements", "vmuc.entitlements", bin/"vmuc"
    (pkgshare/"kernel").install resource("kernel")
    (pkgshare/"templates").install Dir["guest/templates/*"]
    generate_completions_from_executable(bin/"vmuc", "completion")
  end

  def caveats
    <<~EOS
      vmuc needs macOS 15 or newer on Apple silicon. Check with:
        vmuc info
    EOS
  end

  test do
    assert_match "vmuc", shell_output("#{bin}/vmuc --version")
    system "codesign", "-d", "--entitlements", "-", bin/"vmuc"
  end
end
