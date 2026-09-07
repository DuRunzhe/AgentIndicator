class AgentStatusIndicator < Formula
  desc "Native tray monitor for AI coding agents"
  homepage "https://github.com/DuRunzhe/AgentIndicator"
  version "0.2.14"
  license "Apache-2.0"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.14/agent-status-indicator-aarch64-apple-darwin.tar.gz"
      sha256 "a735696b0225d6ef3ad6db56a32a63ba631f64d08c8e7353fad20c34b32a406d"
    else
      url "https://github.com/DuRunzhe/AgentIndicator/releases/download/v0.2.14/agent-status-indicator-x86_64-apple-darwin.tar.gz"
      sha256 "b657454db809939068a1911ca586d55898e14cbbf73de3466f1156b75b6cf3cd"
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
