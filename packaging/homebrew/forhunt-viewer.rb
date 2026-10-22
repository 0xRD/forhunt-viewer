# Homebrew cask template for 0xrd/homebrew-tap (not published yet: B-003).
# Fill version and sha256 from the release's SHA256SUMS.
cask "forhunt-viewer" do
  version "@VERSION@"
  sha256 "@SHA256_DMG@"

  url "https://github.com/0xRD/forhunt-viewer/releases/download/v#{version}/forhunt-viewer-#{version}-macos-arm64.dmg"
  name "forhunt-viewer"
  desc "Fast timeline / CSV viewer for forensic analysts"
  homepage "https://github.com/0xRD/forhunt-viewer"

  depends_on arch: :arm64
  depends_on macos: ">= :big_sur"

  app "forhunt-viewer.app"

  # Unsigned build (B-001): remove the quarantine flag so Gatekeeper opens it.
  postflight do
    system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "#{appdir}/forhunt-viewer.app"]
  end

  zap trash: [
    "~/Library/Application Support/forhunt-viewer",
    "~/Library/Caches/forhunt-viewer",
  ]
end
