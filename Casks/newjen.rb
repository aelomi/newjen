# frozen_string_literal: true

cask "newjen" do
  version "0.1.0-alpha.1"
  sha256 "ed93c2165e42d3edefcb6d21d61c76a7c71d29a85e1a20a1c9b76e45dbf35b4b"

  url "https://github.com/aelomi/newjen/releases/download/v#{version}/Newjen-#{version}-arm64.zip"
  name "Newjen"
  desc "Native agent development environment"
  homepage "https://github.com/aelomi/newjen"

  auto_updates true

  depends_on arch: :arm64
  depends_on macos: :big_sur

  app "Newjen.app"

  postflight_steps do
    run "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "{{appdir}}/Newjen.app"]
  end

  uninstall quit: "com.aelomi.newjen"

  zap trash: [
    "~/Library/Application Support/Newjen",
    "~/Library/Caches/com.aelomi.newjen",
    "~/Library/Preferences/com.aelomi.newjen.plist",
    "~/Library/Saved Application State/com.aelomi.newjen.savedState",
  ]

  caveats <<~EOS
    Newjen is unsigned by Apple and is not notarized. Quarantine is removed on install.
    The app has an ad-hoc signature. Install only if you trust aelomi/newjen.
  EOS
end
