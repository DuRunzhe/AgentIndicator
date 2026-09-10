class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.21"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.21/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "59f9a1ca548031c429427ba2e5f606224c70cb382b7f9c177a2a40b1aebc1729"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.21/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "ccf42591456f5675f79fc54a5fb4c4efb9b14d72279e9482128bef858c078afe"
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
