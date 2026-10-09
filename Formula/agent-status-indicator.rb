class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.29"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.29/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "73f4f7227214bb6e4f866ddd103b48509b68103c6c330d7d841182e9670ee33e"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.29/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "a72d3321ba29dece4a9d18f123311b525a2ec240c9b49e2b82fb5c19a354c738"
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
