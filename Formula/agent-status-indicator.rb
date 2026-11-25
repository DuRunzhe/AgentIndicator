class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.47"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.47/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "18c899c57f4a3253b7aa531605e8c4d346d87b39a3569b1fbc01d82f262658a1"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.47/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "af73e6c25e9b012000bfd2357774842657dc02851ba12666757ac787405f811e"
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
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.47/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ed99ba6e5bfa92f1d91ce0a15fd4a801762e4296db3b8ee0653d16ee87ad1707"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.47/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4ad7205b6be2c0be732c0c7d4d84db69e016f4366bdf06d64438fccc6c5d90f2"
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
