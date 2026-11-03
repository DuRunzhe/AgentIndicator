class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.36-alpha.1"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.36-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "11b1c2895b9f5bd2e1a193d7517b25e12f0935e5c8b0d19c0e9d0d94ce115f0c"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.36-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "ccb2a026abba9e90ccbc892e165986f2ac2b335842e3a9b916084803dc051225"
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
