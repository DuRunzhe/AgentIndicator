class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.38-alpha.5"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.38-alpha.5/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "eb0aa24880e42a939fcf222b8088c8da89e6eab1437b8a2c7dbe88685dc66253"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.38-alpha.5/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "c57cdac6cd383ef42248b4a76636e2e353cc52b12ad582201897be3b60624a56"
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
