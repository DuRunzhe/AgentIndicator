class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.34-alpha.3"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.34-alpha.3/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "82d4c8fdb5e749a1d16fd9721009d000b21e63dd0eccd001349edf2559fd8020"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.34-alpha.3/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "80d995fbd09ffd40854c61802750fc4afc5c144bf8c74ab3793352d7b5df59a3"
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
