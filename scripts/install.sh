#!/usr/bin/env bash
#
# Install the latest open-memory release binary.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/raymondj99/open-memory/main/scripts/install.sh | bash
#
# Environment variables:
#   INSTALL_DIR   Where to place the binary (default: ~/.local/bin)
#   VERSION       Specific version to install (default: latest release)

set -euo pipefail

REPO="raymondj99/open-memory"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

main() {
    check_dependencies
    detect_platform
    resolve_version
    download_and_install
    verify_path
    printf "\nopen-memory %s installed to %s/open-memory\n" "$VERSION" "$INSTALL_DIR"
    printf "Run 'open-memory init' to get started.\n"
}

check_dependencies() {
    local missing=()
    for cmd in curl tar shasum; do
        if ! command -v "$cmd" >/dev/null 2>&1; then
            missing+=("$cmd")
        fi
    done
    if [ ${#missing[@]} -gt 0 ]; then
        printf "error: missing required commands: %s\n" "${missing[*]}" >&2
        exit 1
    fi
}

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin) os="apple-darwin" ;;
        Linux)  os="unknown-linux-gnu" ;;
        *)
            printf "error: unsupported OS: %s\n" "$os" >&2
            exit 1
            ;;
    esac

    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        arm64|aarch64)  arch="aarch64" ;;
        *)
            printf "error: unsupported architecture: %s\n" "$arch" >&2
            exit 1
            ;;
    esac

    TARGET="${arch}-${os}"
}

resolve_version() {
    if [ -n "${VERSION:-}" ]; then
        return
    fi
    printf "Fetching latest release...\n"
    VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' \
        | head -1 \
        | sed 's/.*"tag_name": *"//;s/".*//')"
    if [ -z "$VERSION" ]; then
        printf "error: could not determine latest release\n" >&2
        exit 1
    fi
}

download_and_install() {
    local version_num="${VERSION#v}"
    local archive="open-memory-${version_num}-${TARGET}.tar.gz"
    local url="https://github.com/${REPO}/releases/download/${VERSION}/${archive}"
    local checksum_url="${url}.sha256"

    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    printf "Downloading %s...\n" "$archive"
    curl -fsSL -o "${tmpdir}/${archive}" "$url"
    curl -fsSL -o "${tmpdir}/${archive}.sha256" "$checksum_url"

    printf "Verifying checksum...\n"
    (cd "$tmpdir" && shasum -a 256 -c "${archive}.sha256")

    printf "Extracting...\n"
    tar -xzf "${tmpdir}/${archive}" -C "$tmpdir"

    mkdir -p "$INSTALL_DIR"
    mv "${tmpdir}/open-memory-${version_num}-${TARGET}/open-memory" "$INSTALL_DIR/open-memory"
    chmod +x "$INSTALL_DIR/open-memory"
}

verify_path() {
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            printf "\nNote: %s is not in your PATH.\n" "$INSTALL_DIR"
            printf "Add it with:\n"
            printf "  export PATH=\"%s:\$PATH\"\n" "$INSTALL_DIR"
            ;;
    esac
}

main
