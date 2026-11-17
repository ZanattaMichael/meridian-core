#!/bin/sh
# Installs Meridian on Linux or macOS from a published release.
#
#   curl -fsSL https://github.com/ZanattaMichael/meridian-core/releases/latest/download/install.sh | sh
#
# It downloads the archive for this machine, checks it against the release's
# checksums, and unpacks the meridian command and its plugins together into
# one directory. Nothing is installed outside that directory, and nothing
# needs root.
#
# Settings, all optional, read from the environment:
#
#   MERIDIAN_VERSION        the version to install, as in 0.1.0. Default: the latest release.
#   MERIDIAN_INSTALL_DIR    where to put it. Default: $HOME/.meridian/bin
#   MERIDIAN_VERIFY         "cosign" to refuse to install unless cosign has
#                           verified the checksums' signature; "none" to skip
#                           that check. Default: verify when cosign is installed.
#   MERIDIAN_DOWNLOAD_BASE  where the release's files are, in place of GitHub:
#                           a URL or a directory. For a mirror, or for testing.

set -eu

REPOSITORY="ZanattaMichael/meridian-core"
# Who signs a release: this repository's release workflow, run for a version tag.
SIGNER_IDENTITY="^https://github\\.com/${REPOSITORY}/\\.github/workflows/release\\.yml@refs/tags/v[0-9]"
SIGNER_ISSUER="https://token.actions.githubusercontent.com"

say() { printf '%s\n' "$*"; }
fail() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}
have() { command -v "$1" >/dev/null 2>&1; }

# Copies one of the release's files to a local path. Returns non-zero when
# the release does not have the file.
fetch() {
    case "$base" in
    http://* | https://*)
        if have curl; then
            curl --fail --silent --show-error --location --proto '=https,http' \
                --output "$2" "$base/$1"
        elif have wget; then
            wget --quiet --output-document="$2" "$base/$1"
        else
            fail "curl or wget is needed to download Meridian"
        fi
        ;;
    *)
        [ -f "$base/$1" ] && cp "$base/$1" "$2"
        ;;
    esac
}

sha256_of() {
    if have sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif have shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        fail "sha256sum or shasum is needed to check the download"
    fi
}

case "$(uname -s)" in
Linux) os="unknown-linux-musl" ;;
Darwin) os="apple-darwin" ;;
*) fail "there is no release for $(uname -s); on Windows use install.ps1" ;;
esac
case "$(uname -m)" in
x86_64 | amd64) arch="x86_64" ;;
arm64 | aarch64) arch="aarch64" ;;
*) fail "there is no release for a $(uname -m) processor" ;;
esac
target="$arch-$os"

install_dir="${MERIDIAN_INSTALL_DIR:-$HOME/.meridian/bin}"
verify="${MERIDIAN_VERIFY:-auto}"
version="${MERIDIAN_VERSION:-}"
version="${version#v}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

if [ -n "${MERIDIAN_DOWNLOAD_BASE:-}" ]; then
    base="${MERIDIAN_DOWNLOAD_BASE%/}"
    if [ -z "$version" ]; then
        fetch VERSION "$work/VERSION" || fail "no VERSION file at $base"
        version="$(tr -d ' \r\n' <"$work/VERSION")"
    fi
elif [ -n "$version" ]; then
    base="https://github.com/$REPOSITORY/releases/download/v$version"
else
    base="https://github.com/$REPOSITORY/releases/latest/download"
    fetch VERSION "$work/VERSION" || fail "could not find the latest release of Meridian"
    version="$(tr -d ' \r\n' <"$work/VERSION")"
    base="https://github.com/$REPOSITORY/releases/download/v$version"
fi
case "$version" in
'' | *[!0-9A-Za-z.+-]*) fail "\"$version\" is not a version" ;;
esac

archive="meridian-$version-$target.tar.gz"
checksums="meridian-$version-checksums.txt"
say "Installing Meridian $version for $target"

fetch "$checksums" "$work/$checksums" || fail "could not download $checksums from $base"
fetch "$archive" "$work/$archive" || fail "could not download $archive from $base"

# The checksums are what the archive is checked against, so they are checked
# first: against the signature the release workflow made for them.
case "$verify" in
none) ;;
auto | cosign)
    if ! have cosign; then
        [ "$verify" = cosign ] && fail "MERIDIAN_VERIFY=cosign, and cosign is not installed"
        say "cosign is not installed, so the release's signature was not checked; the download is still checked against its checksums"
    elif ! fetch "$checksums.sigstore.json" "$work/$checksums.sigstore.json"; then
        [ "$verify" = cosign ] && fail "this release has no signature for its checksums"
        say "this release has no signature for its checksums, so none was checked"
    else
        cosign verify-blob \
            --bundle "$work/$checksums.sigstore.json" \
            --certificate-identity-regexp "$SIGNER_IDENTITY" \
            --certificate-oidc-issuer "$SIGNER_ISSUER" \
            "$work/$checksums" >/dev/null 2>"$work/cosign.err" ||
            fail "the checksums' signature does not verify: $(cat "$work/cosign.err")"
        say "The release's signature verifies"
    fi
    ;;
*) fail "MERIDIAN_VERIFY is \"$verify\"; use cosign or none, or leave it unset" ;;
esac

expected="$(awk -v name="$archive" '$2 == name { print $1 }' "$work/$checksums")"
[ -n "$expected" ] || fail "$checksums does not list $archive"
actual="$(sha256_of "$work/$archive")"
[ "$expected" = "$actual" ] ||
    fail "$archive does not match its checksum (expected $expected, got $actual); nothing was installed"

mkdir -p "$work/unpacked"
tar -xzf "$work/$archive" -C "$work/unpacked"
unpacked="$work/unpacked/meridian-$version-$target"
[ -x "$unpacked/meridian" ] || fail "$archive does not hold a meridian command"

# Plugins from an earlier version are removed, so a plugin that a release
# dropped does not linger beside a meridian that never shipped with it.
mkdir -p "$install_dir"
rm -f "$install_dir"/meridian-target-*
cp -f "$unpacked"/* "$install_dir"/

"$install_dir/meridian" --version >/dev/null ||
    fail "the installed meridian does not run on this machine"

say "Installed $("$install_dir/meridian" --version) in $install_dir"
case ":$PATH:" in
*":$install_dir:"*) ;;
*)
    say ""
    say "$install_dir is not on your PATH. Add it, for example with:"
    say "  export PATH=\"$install_dir:\$PATH\""
    ;;
esac
