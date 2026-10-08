#!/bin/sh
# Install pm from its GitHub releases: README, "Install", has the usage.
#
#   PM_VERSION      the release to install (default: the latest)
#   PM_INSTALL_DIR  where to put `pm` (default: beside an existing pm on
#                   PATH if writable, else ~/.local/bin)
#   PM_DOWNLOAD_URL where the release's assets are (a test seam)
#
# The binary is checked against the release's SHA256SUMS. Safe to re-run:
# it replaces pm in place and upgrades every project.
set -eu

repo="https://github.com/ciaranbor/pm"

fail() {
    echo "pm install: $*" >&2
    exit 1
}

target() {
    os=$(uname -s)
    arch=$(uname -m)
    # A shell under Rosetta reports x86_64 on Apple silicon.
    if [ "$os" = Darwin ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
        arch=arm64
    fi
    case "$os/$arch" in
        Darwin/arm64) echo aarch64-apple-darwin ;;
        Linux/x86_64) echo x86_64-unknown-linux-gnu ;;
        *) fail "no prebuilt binary for $os $arch; build from source: cargo install --git $repo" ;;
    esac
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        fail "neither curl nor wget is installed"
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        fail "neither sha256sum nor shasum is installed"
    fi
}

install_dir() {
    if [ -n "${PM_INSTALL_DIR:-}" ]; then
        echo "$PM_INSTALL_DIR"
        return
    fi
    existing=$(command -v pm 2>/dev/null || true)
    case "$existing" in
        /*)
            dir=$(dirname "$existing")
            if [ -w "$dir" ]; then
                echo "$dir"
                return
            fi
            ;;
    esac
    echo "$HOME/.local/bin"
}

target=$(target)
asset="pm-$target"
if [ -n "${PM_DOWNLOAD_URL:-}" ]; then
    url="$PM_DOWNLOAD_URL"
elif [ -n "${PM_VERSION:-}" ]; then
    url="$repo/releases/download/v${PM_VERSION#v}"
else
    url="$repo/releases/latest/download"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if ! fetch "$url/$asset" "$work/$asset"; then
    # The release workflow adds the macOS binary after the rest.
    pending=
    case "$target" in
        *apple-darwin) pending="if it was published in the last hour, try again later; otherwise " ;;
    esac
    fail "could not download $url/$asset; ${pending}build from source: cargo install --git $repo"
fi
fetch "$url/SHA256SUMS" "$work/SHA256SUMS" || fail "could not download $url/SHA256SUMS"
expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS")
[ -n "$expected" ] || fail "SHA256SUMS lists no $asset"
actual=$(sha256 "$work/$asset")
[ "$actual" = "$expected" ] || fail "$asset does not match SHA256SUMS (expected $expected, got $actual); nothing was installed"

dir=$(install_dir)
mkdir -p "$dir"
pm="$dir/pm"
replacing=false
[ -e "$pm" ] && replacing=true
# Renamed into place, never written over: a running pm keeps its file, and
# macOS kills processes whose signed binary is overwritten in place.
tmp="$dir/.pm-new-$$"
trap 'rm -rf "$work" "$tmp"' EXIT
cp "$work/$asset" "$tmp"
chmod 755 "$tmp"
mv -f "$tmp" "$pm"
echo "Installed $("$pm" --version) to $pm"

case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "warning: $dir is not on your PATH; add it in your shell's profile" >&2 ;;
esac

if $replacing; then
    # A PM_VERSION whose `pm upgrade` covers only the project it runs in
    # lists `--all` in its help; later ones upgrade every project anyway.
    if "$pm" upgrade --help | grep -q -- --all; then
        "$pm" upgrade --all
    else
        "$pm" upgrade
    fi
else
    cat <<EOF

Next, add pm's tmux plugin, one line in your tmux config:

    run-shell 'pm tmux init'

See $repo#install for the rest.
EOF
fi
