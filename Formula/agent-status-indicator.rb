class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.17"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.17/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "c7cb257c7541bd799ad46d5edd9141e45ab0e89cc7e8036717e0446600682076"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.17/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "a6ef14c8967ea39e446f82a4abacad327001b3132374feaeb06b55388ecd91b5"
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
