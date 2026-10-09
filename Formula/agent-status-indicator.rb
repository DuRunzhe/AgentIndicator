class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.29-alpha.2"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.29-alpha.2/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "428d6a1b3f99497bbf767e03de37270dc8f976b342cafbd5bd2c8b533fe380c2"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.29-alpha.2/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "a88a8210fb25bcd83f566b366053ab6d80b312e31fbcb8f633b25eeced8c83e8"
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
