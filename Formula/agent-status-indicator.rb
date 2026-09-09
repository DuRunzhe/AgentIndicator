class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.18"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.18/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "ae03082dd80476ea0f308037a324e618b506abf152da6e0422fba19aaf3551e2"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.18/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "68e061bef5980afe184a3497a39311e341e7f4e5cd293b1dbc00298cf6b95ac4"
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
