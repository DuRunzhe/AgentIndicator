class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.11"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.11/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "7ca37ebc1b09d02843dc0138f08f8b783a0e5ac10412d4179e84e58a40b60045"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.11/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "25dc14079c6040ed277a6c007c10a9f8e2fbef6b924a4ce03b86f18e9953bb12"
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
