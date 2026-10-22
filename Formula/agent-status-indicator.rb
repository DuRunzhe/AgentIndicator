class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.34-alpha.2"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.34-alpha.2/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "1017d501cb90acec84873b22e105cb453440a404885aa94b4560cdd5dd84195c"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.34-alpha.2/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "d9e1841ce7e4a71a895f7ba9daf18fa1df01088608cc32049e20716428c64df8"
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
