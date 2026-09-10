class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.22"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.22/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "1a6e21e0976f75837b7cdee172b84f22cddac61f3a15b17dbc9cd33db38034d3"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.22/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "b065f57f30d88192d904ad3c717ae269efb84d7fe9d0f58eeecb3dd4b54df232"
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
