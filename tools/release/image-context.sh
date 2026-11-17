#!/bin/sh
# Lays out the container image's build context from the two Linux archives:
# <out>/amd64 and <out>/arm64, each the unpacked contents of one archive.
#
#   tools/release/image-context.sh <version> <directory of archives> <out>
set -eu

version="$1"
archives="$2"
out="$3"

mkdir -p "$out"
for pair in amd64:x86_64 arm64:aarch64; do
    arch="${pair%%:*}"
    name="meridian-$version-${pair##*:}-unknown-linux-musl"
    tar -xzf "$archives/$name.tar.gz" -C "$out"
    rm -rf "${out:?}/$arch"
    mv "$out/$name" "$out/$arch"
done
