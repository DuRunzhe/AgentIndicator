class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.41"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.41/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "b5b1f8a4e4abcbacc478498709e819f229f5bf573e52efd40bdff58bae1fd7bd"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.41/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "d41aa8d0a553df25a3320464986be025196ad7cb1ee359541393cf2557640b4b"
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
