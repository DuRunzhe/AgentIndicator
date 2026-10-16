class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.32-alpha.4"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.32-alpha.4/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "987095d9e1850b692f1eadee67561387fdab2d49dc5f2a8f829067e8a36c2bdd"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.32-alpha.4/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "c8cb0c5c24bcde0cac37932a876611b049b058800d60d874833c6322d43433dd"
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
