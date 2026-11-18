class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.43"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.43/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "cf8f1dec198a6b16514fbc1c9335923bf40ee5e9ab079deae64698843391b3f5"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.43/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "5dc7a271620a572edf06e5d05d85c90a47360a91bf35d9d875410d1f2b9baca9"
    end
  end

  on_linux do
    # The tray backend dlopen()s libayatana-appindicator at runtime, so the
    # library has to come from the formula rather than from the host distro.
    depends_on "libayatana-appindicator"
    # Runtime helpers that degrade gracefully when missing: xdotool gives
    # precise X11 terminal focus, zstd reads DeepSeek Harness' compressed
    # session logs.
    depends_on "xdotool"
    depends_on "zstd"

    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.43/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7f1593ed4bdf70590ba00ca198098e2b1d61bff114c59899b9a2996b33c09c9c"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.43/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c55916c2811ac80e3e335a2db4e4eb22699dae478ea5d97b7fb5ad4fe4c74c4b"
    end
  end

  def install
    bin.install "agent-status-indicator"
  end

  service do
    run [opt_bin/"agent-status-indicator"]
    keep_alive true
    log_path var/"log/agent-status-indicator.log"
    error_log_path var/"log/agent-status-indicator.log"
  end
end
