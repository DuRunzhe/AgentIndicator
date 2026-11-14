class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.39"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.39/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "2289be30b5a9ae2a68c7936429ad84a3e1891185eb3707e88494a519aa48a004"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.39/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "4554a81c7b21a71e2e9c30af6f7e817fa01fe35eacd90402d3e42de9b80f0047"
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
