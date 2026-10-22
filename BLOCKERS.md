# Blockers

Things that cannot be done from the build environment or need the owner.

| # | Blocker | Impact / workaround | Status |
|---|---------|---------------------|--------|
| B-001 | No Apple Developer ID (user decision). | macOS .app/.dmg are unsigned and not notarized; Gatekeeper blocks a double-click on first launch. README: right-click > Open, or `xattr -dr com.apple.quarantine "/Applications/forhunt-viewer.app"`. Add `APPLE_*` secrets + a codesign/notarytool step to enable. | open |
| B-002 | No Windows code-signing certificate (user decision). | SmartScreen warns on the MSI and exe. README documents "More info > Run anyway". | open |
| B-003 | Homebrew cask in `0xrd/homebrew-tap` needs a tap token secret (user decision: later). | Cask template in `packaging/homebrew/`; publish by hand or add `HOMEBREW_TAP_TOKEN` and a release job. | open |
| B-004 | No macOS or Windows machine in the build container. | macOS packaging, the Finder "Open With" handler (D-026), Windows MSI/Open-with registration and the Windows icon resource are type-checked here but run for the first time in the release workflow. Owner: please try "Open With" on macOS and Windows after the first release. | open |
