class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.34-alpha.1"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.34-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "7c34ed00c0ba8d9de9b9ba7ef756b69129233d4d2e4bdf26ba31ec76bab4274b"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.34-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "27dd98b4cbbb2f99be8e2198536363a758805cfc6e82bc728dc2d196adc5761c"
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
