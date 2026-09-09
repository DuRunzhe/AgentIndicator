class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.16"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.16/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "74b656fb5cea3795d4f742a8273145475d7f7bf81c61a6b59e0bb79e7d2ba82a"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.16/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "f7cceb06a2d82f485a821e91e953184ce0331b9b14cf501f937f254e70753810"
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
