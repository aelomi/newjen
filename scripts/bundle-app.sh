#!/bin/bash
set -euo pipefail

if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo 'Newjen bundles require an Apple Silicon Mac.' >&2
    exit 1
fi
if [[ ! -f Cargo.toml || ! -d crates/app || $# -gt 1 ]]; then
    echo 'Usage from the repo root: scripts/bundle-app.sh [version]' >&2
    exit 1
fi
version=${1:-$(awk '/^\[workspace.package\]$/ { package = 1; next } /^\[/ { package = 0 } package && /^version = / { gsub(/"/, "", $3); print $3; exit }' Cargo.toml)}
if [[ ! $version =~ ^([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo 'Version must have the form 0.1.0 or 0.1.0-alpha.1.' >&2
    exit 1
fi
bundle_version=${BASH_REMATCH[1]}
export MACOSX_DEPLOYMENT_TARGET=11.0
target=aarch64-apple-darwin
binary="target/$target/release/newjen"
cargo build --release --locked --package app --bin newjen --features gpui_platform/runtime_shaders --target "$target" --target-dir target
[[ $(lipo -archs "$binary") == arm64 ]] || {
    echo 'The release binary must contain only arm64 code.' >&2
    exit 1
}

mkdir -p dist
stage=$(mktemp -d dist/.bundle.XXXXXX)
trap 'rm -rf "$stage"' EXIT
app="$stage/Newjen.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/newjen"
cp packaging/macos/Info.plist "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $bundle_version" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $bundle_version" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :NewjenReleaseVersion $version" "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
mkdir "$stage/Newjen.iconset"
xcrun swift packaging/macos/icon.swift "$stage/Newjen.iconset"
iconutil -c icns "$stage/Newjen.iconset" -o "$app/Contents/Resources/Newjen.icns"
cp crates/app/THIRD-PARTY-NOTICES.md "$app/Contents/Resources/"
printf 'APPL????' > "$app/Contents/PkgInfo"
codesign --force --deep -s - "$app"
codesign --verify --deep --strict "$app"
rm -rf dist/Newjen.app
mv "$app" dist/Newjen.app
archive="dist/Newjen-$version-arm64.zip"
rm -f "$archive"
ditto -c -k --keepParent dist/Newjen.app "$archive"
printf 'Built dist/Newjen.app and %s\n' "$archive"
