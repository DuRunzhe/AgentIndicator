class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.38-alpha.2"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.38-alpha.2/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "4ef2ca3e7283bb956abc2af5dbcea02a424f5f5ed9282aafb1ab044a1ac8802a"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.38-alpha.2/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "f5ad5630bc719305d722d4cac4930b6d18b2d969b382068446a6d008ac8a47ad"
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
