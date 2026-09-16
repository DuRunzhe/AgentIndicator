class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.24-alpha.2"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.24-alpha.2/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "d24b2bca6864d4a4215dec5c6eb85401b318c72063f72ce4d19e6133fd15be43"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.24-alpha.2/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "759b6fee9ebebfbe2361fd0374f1ef43fa919155fe94bd7b71e942f2d8eda28f"
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
