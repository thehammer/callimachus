class Callimachus < Formula
  desc "Queryable index over books, code, and wikis — exposed as LLM tools via MCP"
  homepage "https://github.com/thehammer/callimachus"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/thehammer/callimachus/releases/download/v#{version}/calli-aarch64-apple-darwin"
      sha256 "PLACEHOLDER_SHA256_AARCH64"
    else
      url "https://github.com/thehammer/callimachus/releases/download/v#{version}/calli-x86_64-apple-darwin"
      sha256 "PLACEHOLDER_SHA256_X86_64"
    end
  end

  def install
    bin.install Dir["calli-*"].first => "calli"
  end

  test do
    assert_match "callimachus", shell_output("#{bin}/calli --version")
  end
end
