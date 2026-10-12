class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.31"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.31/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "c0b84d3e85ed55f2fb33001922d44835a666cf71b4d8f08426935adbc034566d"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.31/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "6d55cdb80f18212b92644cde40c2bc1d4f061561a5db31d469a35f381c574fd8"
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
