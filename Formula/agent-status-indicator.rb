class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.32-alpha.5"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.32-alpha.5/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "afc1fcf8ae9bb2073f03621b4a6e47a128c072c5083ca447ef8676b300528859"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.32-alpha.5/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "022ae3dae4b6acc5184c5ed10f2684bc89aa65a75a85fb893414f8a8709d206c"
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
