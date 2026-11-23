#!/usr/bin/env bash
set -euo pipefail
version=${1:?supply the package version}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
install -d "$stage"/{DEBIAN,usr/bin,usr/share/applications,usr/share/doc/daedalus}
for program in daedalus-ui daedalus-ctl daedalusd; do
    install -m755 "target/release/$program" "$stage/usr/bin/$program"
done
install -m644 LICENSE "$stage/usr/share/doc/daedalus/copyright"
install -m644 packaging/daedalus.desktop "$stage/usr/share/applications/daedalus.desktop"
# Resolve the actual linked library versions from the release binaries.
mkdir -p "$stage/build/debian"
touch "$stage/build/debian/control"
deps=$(cd "$stage/build" && dpkg-shlibdeps -O -e../usr/bin/daedalus-ui -e../usr/bin/daedalusd -e../usr/bin/daedalus-ctl)
rm -r "$stage/build"
cat > "$stage/DEBIAN/control" <<CONTROL
Package: daedalus
Version: $version
Architecture: $(dpkg --print-architecture)
Maintainer: AraneaDev <12177132+AraneaDev@users.noreply.github.com>
Section: devel
Priority: optional
Depends: ${deps#shlibs:Depends=}, libxkbcommon0, libwayland-client0, libx11-6
Description: Native coding-agent and worktree workbench
CONTROL
mkdir -p dist
dpkg-deb --root-owner-group --build "$stage" "dist/daedalus-$version-$(dpkg --print-architecture).deb"
