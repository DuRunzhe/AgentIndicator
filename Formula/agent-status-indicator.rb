class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.36-alpha.2"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.36-alpha.2/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "418f3d65e1ee668710837ebdd455e299149ba63f48a92aff1d54b2a84604ec36"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.36-alpha.2/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "2fc9575f3d2d36d5c59bd1ad01be80ea3490df3c2d6a47366e69d53ce3bf0fcb"
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
