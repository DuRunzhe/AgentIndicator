class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.38-alpha.1"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.38-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "6a18e6609c206c1d07eb8ddaaba40fd88ee30192a1ba94e3ec8a4b6674bc6bd2"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.38-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "764afb0b55bb660db4053252716abf5b3b2e5d153706c3ffb333a7982eb1fba9"
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
