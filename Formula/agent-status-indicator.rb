class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.30"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.30/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "259a89290b83d7db1b1a997e9d1c0d9ab462edad870f8bd812c33eb6c5d63e6d"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.30/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "71c39125e7ccf53101fa75c208c6609207895251da64a75f7e7150877cb9d210"
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
