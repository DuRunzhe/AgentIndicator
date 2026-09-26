class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.28"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.28/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "30364fc51d122f7ae518519da250af272674b3b5bd388831c78c988c957ebfc9"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.28/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "0a7c6ee1b0b3f703e681d5e0966051546b581056bc9c5db37f5be00076d97d38"
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
