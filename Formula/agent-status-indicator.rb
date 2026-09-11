class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.23"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.23/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "4fadb1b9eaa1116b3e1dc2aa5cf4a8084409019848d77d95f0c3183c56f6d49b"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.23/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "086b4f0ae7d1e4159235de4fd56ee0b4ea7994d5683b2d3fe858569af5adc369"
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
