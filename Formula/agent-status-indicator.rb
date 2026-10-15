class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.32-alpha.2"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.32-alpha.2/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "65b85acbc00fb732d19da3c632be7d64cc36f0d3fd0f7a04d5376d95ef146746"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.32-alpha.2/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "b3b1b24bc2e9675a1b83369447c14b41ccf100c895acd53ce273c8ca4ad5d2f8"
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
