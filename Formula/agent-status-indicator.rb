class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.20"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.20/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "702c0acb2a5a8f041453d6530a57fce646afacf91b7939d084128dc8006dcad2"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.20/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "81f261526879ccf92d95037bc1e04c7922778e17d3956ebfd3d749f5bcfbdb3b"
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
