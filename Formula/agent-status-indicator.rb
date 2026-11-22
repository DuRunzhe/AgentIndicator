class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.50-alpha.2"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.50-alpha.2/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "aae1b93678f7bdf1c6907aa4c6d954b6ff51aa235b7f400b9ed2320375f61a2c"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.50-alpha.2/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "bde9a0e976cbb797767656edf0af140194ab4aa48ca8470c55c29cb63e7a06ad"
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
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.50-alpha.2/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6dd09491cd74f8cbe1b74a2789b177b05ce9839524737d722dd4ed3896c67a69"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.50-alpha.2/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "afcd5482d3e11c40018c5309ff6c80b354c20d5e8c031ff89621a69d46d4b2dd"
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
