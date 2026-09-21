class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.27-alpha.1"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.27-alpha.1/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "ecfb61584e3962baba97b74def02f0ddf2f3ecaaaabd8115a02dc7651bc6d98b"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.27-alpha.1/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "d020456f59b6a28b881c2739fe8165fca77671fdb1c9a0b79737ddb913c205a0"
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
