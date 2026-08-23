class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentStatusIndicator"
  version "0.2.9"
  license "MIT"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentStatusIndicator/releases/download/v0.2.9/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_ON_RELEASE"
    else
      url "https://github.com/DuRunzhe/AgentStatusIndicator/releases/download/v0.2.9/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_ON_RELEASE"
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
