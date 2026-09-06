class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.13"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.13/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "24c9eb351932074b9c1e4e233d504339ae8003dadb816b0776a75057e415226a"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.13/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "0520f7806387788ba506fb19a3090208b163e54894df98977fbd3ae606f567a9"
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
