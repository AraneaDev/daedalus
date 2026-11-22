#!/usr/bin/env bash
set -euo pipefail
version=${1:?supply the package version}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
app="$stage/Applications/Daedalus.app"
mkdir -p "$app/Contents/MacOS" "$stage/usr/local/bin" dist
for program in daedalus-ui daedalusd daedalus-ctl; do
    install -m755 "target/release/$program" "$app/Contents/MacOS/$program"
    ln -s "/Applications/Daedalus.app/Contents/MacOS/$program" "$stage/usr/local/bin/$program"
done
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.aranea.daedalus</string>
<key>CFBundleName</key><string>Daedalus</string>
<key>CFBundleExecutable</key><string>daedalus-ui</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleShortVersionString</key><string>$version</string>
</dict></plist>
PLIST
plutil -lint "$app/Contents/Info.plist"
pkgbuild --root "$stage" --install-location / --identifier dev.aranea.daedalus.pkg \
  --version "$version" "dist/daedalus-$version-$(uname -m).pkg"
