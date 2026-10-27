class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.35"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.35/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "a43614c3572b2c43014a2a3467b91b259ff4e981a738b5f18f821140f0e7bb03"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.35/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "939a43fe20091af48473e3db4319ad6dfefe808d758fedccfb504bc0e737cfab"
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
