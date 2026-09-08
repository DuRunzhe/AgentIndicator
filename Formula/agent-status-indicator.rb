class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.15"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.15/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "42b55f112c3bca20c6cb62160c109c64b807b95128c8cd76157a71c7f768323a"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.15/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "83e2659cfb0206738951f0910c239a389fe9094a3c4afda390cb618461356ed4"
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
