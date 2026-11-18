class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.46-alpha.1"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.46-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "5aa919c865479327f4eba735d7c72a7fbfe63c43e19c7ee5f4f22392474a5edf"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.46-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "31362c2ad470eb39bfd30dab6d825f70586e4970972aa7b2773e90ada3a060da"
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
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.46-alpha.1/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "86039e23c4725002aeef52a77497186ebef52f3e4c024864e39a099e1e92ba57"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.46-alpha.1/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0e362f804652e8373f07203e633f2f54b22a79b2358c7cb92c08c24d42373ee9"
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
