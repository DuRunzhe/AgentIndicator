class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.10"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.10/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "545f3a0c90ae9d7945e0212c200e8dbd0873e08f454c1f560020259d8843e800"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.10/agent-status-indicator-x86_64-apple-darwin.tar.gz"
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
