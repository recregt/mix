#!/bin/sh
set -eu

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

main() {
    if [ -n "${MIX_DOWNLOAD_URL:-}" ]; then
        base=${MIX_DOWNLOAD_URL%/}
    elif [ -n "${MIX_VERSION:-}" ]; then
        base="https://github.com/recregt/mix/releases/download/$MIX_VERSION"
    else
        base="https://github.com/recregt/mix/releases/latest/download"
    fi

    [ "$(uname -s)" = Linux ] || fail "mix runs on Linux only"
    [ -d /run/systemd/system ] || fail "mix needs a system that runs systemd"

    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) fail "there is no mix release for $(uname -m)" ;;
    esac

    case "$base" in
        https://*) secure=yes ;;
        *) secure=no ;;
    esac
    if command -v curl >/dev/null 2>&1; then
        if [ "$secure" = yes ]; then
            fetch() { curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"; }
        else
            fetch() { curl -fsSL -o "$2" "$1"; }
        fi
    elif command -v wget >/dev/null 2>&1; then
        if [ "$secure" = yes ]; then
            fetch() { wget -q --https-only -O "$2" "$1"; }
        else
            fetch() { wget -q -O "$2" "$1"; }
        fi
    else
        fail "downloading mix needs curl or wget"
    fi

    if [ "$(id -u)" -eq 0 ]; then
        as_root() { "$@"; }
    elif command -v sudo >/dev/null 2>&1; then
        as_root() { sudo "$@"; }
    else
        fail "installing into /usr/local/bin needs root or sudo"
    fi

    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    archive="mix-$arch-linux.tar.gz"
    printf 'Downloading %s from %s\n' "$archive" "$base"
    fetch "$base/$archive" "$work/$archive" || fail "could not download $base/$archive"
    fetch "$base/$archive.sha256" "$work/$archive.sha256" ||
        fail "could not download $base/$archive.sha256"

    [ "$(awk '{ print $2 }' "$work/$archive.sha256")" = "$archive" ] ||
        fail "$archive.sha256 does not name $archive"
    (cd "$work" && sha256sum -c --status "$archive.sha256") ||
        fail "$archive does not match its published checksum, so nothing was installed"

    tar -xzf "$work/$archive" -C "$work"
    as_root install -d -m 0755 /usr/local/bin
    as_root install -m 0755 "$work/mix-$arch-linux/mix" "$work/mix-$arch-linux/mix-daemon" /usr/local/bin/

    printf '\nInstalled %s into /usr/local/bin.\n\n' "$(/usr/local/bin/mix --version)"
    printf 'See what setup would do, then do it:\n\n  mix bootstrap --dry-run\n  mix bootstrap\n'
}

main "$@"
