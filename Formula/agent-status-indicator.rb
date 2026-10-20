class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.33"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.33/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "205b67d983a00bf1eb9e1bd0b4a55785cca28f69e2dc7ed9dea4919dbf4f8de6"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.33/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "47505174658616fa4e0117b41d4b28aa58873ed5da3518db5bb02845ab8e236d"
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
