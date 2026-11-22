class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.50-alpha.1"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.50-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "848b8ac8c20e4977937960af500dc97940c8e1d4956c817858cf028e2ca6c4fe"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.50-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "4bbf8b519da5d9e9fbc8399aeb914db095aa8c937e39f4f92cce2878ec7a66f9"
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
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.50-alpha.1/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b9f05335d4f4137d3b46bb441e03748e37844b9cf1e2668f4dfbacaae19aadf4"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.50-alpha.1/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d57fd9d6a7a5c3378e5bec26809f72762384db4d0ed0ce0dd1d254622356f30b"
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
