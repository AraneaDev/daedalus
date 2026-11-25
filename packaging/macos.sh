#!/usr/bin/env bash
set -euo pipefail
version=${1:?supply the package version}
arch=${2:?supply the target architecture (arm64 or x86_64)}
case "$arch" in
    arm64|x86_64) ;;
    *) echo "Unsupported macOS architecture: $arch" >&2; exit 1 ;;
esac
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
app="$stage/Applications/Daedalus.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$stage/usr/local/bin" dist
install -m644 packaging/daedalus.icns "$app/Contents/Resources/daedalus.icns"
for program in daedalus-ui daedalusd daedalus-ctl; do
    lipo "target/release/$program" -verify_arch "$arch"
    install -m755 "target/release/$program" "$app/Contents/MacOS/$program"
    ln -s "/Applications/Daedalus.app/Contents/MacOS/$program" "$stage/usr/local/bin/$program"
done
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.aranea.daedalus</string>
<key>CFBundleName</key><string>Daedalus</string>
<key>CFBundleIconFile</key><string>daedalus.icns</string>
<key>CFBundleExecutable</key><string>daedalus-ui</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleShortVersionString</key><string>$version</string>
</dict></plist>
PLIST
plutil -lint "$app/Contents/Info.plist"
pkgbuild --root "$stage" --install-location / --identifier dev.aranea.daedalus.pkg \
  --version "$version" "dist/daedalus-$version-$arch.pkg"
