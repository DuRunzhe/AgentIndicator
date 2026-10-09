class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.29-alpha.1"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.29-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "785ff119a7fa6330926201f144a067a71091d790d6595a7247f8c3541ffb6cd2"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.29-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "543c1b69a4f3d628db3a5ba953f6d9924be351fc7a32ce9cf2cad16ef42eb3c4"
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
