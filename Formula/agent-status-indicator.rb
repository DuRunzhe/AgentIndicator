class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.37"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.37/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "9ed98c221a18ea024c7bd3c2ce2ec809b5ac0e12b4b256b3c331b26881d953d8"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.37/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "85d543833f558884a6e40fe906bcc7e9725479e9887ef8732a11dd5fd66f99a0"
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
