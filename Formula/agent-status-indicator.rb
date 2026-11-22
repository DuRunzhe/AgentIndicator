class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.48-alpha.1"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.48-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "a7b7653f4da5c4c2892ca0bd95f6ff44dc02d16ff89f7ce16349661f9ce7e7d5"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.48-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "063d36ae0be947a2802fb7d84f979f75df5d932946cc7c773b6961089a543c53"
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
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.48-alpha.1/agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5779e7eab74cb918a5a429a3d507c1516c2993d97326d34471532e14eef99ffa"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.48-alpha.1/agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "cf0d8fcd74b2bc1173949205561d4bbe9e052f59e5b435fa36af0b6babc6910e"
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
