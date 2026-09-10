class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.19"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.19/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "3b3412ceabda3b28d6f9d921df85ff2d312560b3180f24632888bb900923dd8a"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.19/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "93b84ab1415fb734f06580ec1771276c2e719d00e9cbf39eca87104810497747"
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
