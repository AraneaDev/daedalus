#!/usr/bin/env bash
set -euo pipefail
version=${1:?supply the package version}
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
binaries=${2:-target/release}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$binaries"/{daedalus-ui,daedalus-ctl,daedalusd} LICENSE packaging/daedalus.desktop packaging/daedalus.png "$stage/"
sed "s/@VERSION@/$version/" packaging/arch/PKGBUILD.in > "$stage/PKGBUILD"
mkdir -p dist
docker run --rm --platform linux/amd64 -v "$stage:/input:ro" -v "$PWD/dist:/output" archlinux:base bash -euc '
    pacman -Syu --needed --noconfirm base-devel
    useradd -m builder
    cp /input/* /home/builder/
    cd /home/builder
    chown builder:builder ./*
    # Package prebuilt binaries, then let pacman resolve and install their runtime dependencies.
    runuser -u builder -- makepkg -g >> PKGBUILD
    runuser -u builder -- makepkg --nodeps
    pkg="daedalus-$1-1-x86_64.pkg.tar.zst"
    pacman -U --noconfirm "./$pkg"
    for program in daedalus-ui daedalus-ctl daedalusd; do "/usr/bin/$program" --help >/dev/null; done
    pacman -Qk daedalus
    pacman -R --noconfirm daedalus
    install -m644 "$pkg" /output/
' bash "$version"
