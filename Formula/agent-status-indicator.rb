class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.40-alpha.1"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.40-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "43c77a3501ea6ee307a2468ae716afa42dcef4a53eef853c8905c8942db2dd68"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.40-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "080524223d7fa93c130a1ddcbafd43abf55a10c9efbe84ec7fd13a1cc91d5f1d"
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
