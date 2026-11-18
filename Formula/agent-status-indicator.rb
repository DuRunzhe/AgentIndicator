class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.44-alpha.1"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.44-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "6fe38f39e8f71f91fbe030b9bad0e9634138c6fa79346a45368862ce089d726a"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.44-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "1721bdc9034c34682854de328f305cad092b784dc27665f2adebffc50a4ba2b2"
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
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.44-alpha.1/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7db5dcfb2a87744c3401561d3af4d3b889cb71f62187ed7def0c28b9bd23222a"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.44-alpha.1/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "da727616bb7ad9d2d1203ba0d043fb6228c5e4010f8c94e6176adac88bf72f03"
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
