class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.38-alpha.3"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.38-alpha.3/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "c9b19dab82b4e1c4f94e8901599b4ddfa3bba296351d4ec9a952c932f7513381"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.38-alpha.3/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "e736afc7373444c9d80902ff931d0afc1535e17d86b1e79bb84c2bf0bc7d5779"
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
