class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.12"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.12/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "4f83b5ea3e7ec327f2e6cef8d474231d7cf5c577999cc1e345a9f24995efd845"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.12/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "360941cbfcb09afb745485d7fe86baea460619b3f8fa7583f23a304ea140d638"
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
