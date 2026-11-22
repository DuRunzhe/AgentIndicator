class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.46-alpha.2"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.46-alpha.2/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "f3f7f67b2397d20f391d7feb0d35126209317c5d1bbb57777b9c17a72f9df978"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.46-alpha.2/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "00fb7d1dc3a51e833ed27780f2029c12c762a883ac50191db19011bb170d18db"
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
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.46-alpha.2/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "24f6a08181ae3d9a607d1ba697b587263a062e3b96d600dd786282a777009914"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.46-alpha.2/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "21e93c0887b0a2ab6d200373779f2695e83be6d11c9217d32fa0eecfefb8889f"
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
