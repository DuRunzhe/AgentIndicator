class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.52-alpha.1"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.52-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "9a4f557b017338d885fe1c3f667b194d84e840dd7060ceab113820f9db47f2e8"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.52-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "1a86fdb1e7df8a3929809f037c736da9cbfc8e599279ec40568f8023347e2025"
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
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.52-alpha.1/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f2c4feca4e7607f58b9da9c3ab75b1f43a51f10cbf31d84e1646d584e07ad970"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.52-alpha.1/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d0498b6891897b89bc89a423f0e7f5b77237eed099ce2a51c0d281eb3bfda9db"
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
