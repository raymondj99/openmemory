#!/usr/bin/env bash
#
# Install the latest openmemory release binary.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/raymondj99/openmemory/main/scripts/install.sh | bash
#
# Environment variables:
#   INSTALL_DIR   Where to place the binary (default: ~/.local/bin)
#   VERSION       Specific version to install (default: latest release)

set -euo pipefail

REPO="raymondj99/openmemory"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

main() {
    check_dependencies
    detect_platform
    resolve_version
    download_and_install
    verify_path
    printf "\nopenmemory %s installed to %s/openmemory\n" "$VERSION" "$INSTALL_DIR"
    run_setup_or_hint
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
    local archive="openmemory-${version_num}-${TARGET}.tar.gz"
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
    mv "${tmpdir}/openmemory-${version_num}-${TARGET}/openmemory" "$INSTALL_DIR/openmemory"
    chmod +x "$INSTALL_DIR/openmemory"
}

verify_path() {
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*)
            PATH_ALREADY_OK=1
            return
            ;;
    esac
    PATH_ALREADY_OK=0

    # Detect the user's shell and patch the matching rc file so the
    # next shell sees the new PATH. We only append once and we always
    # print the line we added.
    local shell_name rc_file line
    shell_name="$(basename "${SHELL:-}")"
    line="export PATH=\"${INSTALL_DIR}:\$PATH\""

    case "$shell_name" in
        zsh)  rc_file="$HOME/.zshrc" ;;
        bash) rc_file="$HOME/.bashrc" ;;
        fish)
            rc_file="$HOME/.config/fish/config.fish"
            line="set -gx PATH \"${INSTALL_DIR}\" \$PATH"
            ;;
        *)    rc_file="" ;;
    esac

    if [ -n "$rc_file" ] && [ -f "$rc_file" ] && grep -Fq "$INSTALL_DIR" "$rc_file" 2>/dev/null; then
        printf "\nNote: %s is not on PATH in this shell, but %s already mentions it.\n" \
            "$INSTALL_DIR" "$rc_file"
        printf "Open a new shell or run: exec %s -l\n" "$shell_name"
        return
    fi

    if [ -n "$rc_file" ]; then
        mkdir -p "$(dirname "$rc_file")"
        {
            printf "\n# Added by openmemory installer (%s)\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
            printf "%s\n" "$line"
        } >> "$rc_file"
        printf "\nAdded to %s:\n  %s\n" "$rc_file" "$line"
        printf "Open a new shell or run: exec %s -l\n" "$shell_name"
    else
        printf "\nNote: %s is not on your PATH.\n" "$INSTALL_DIR"
        printf "Add this to your shell rc file:\n  %s\n" "$line"
    fi

    # Make the binary resolvable in *this* shell so run_setup_or_hint
    # can chain directly into `openmemory setup`.
    export PATH="${INSTALL_DIR}:${PATH}"
}

run_setup_or_hint() {
    if command -v openmemory >/dev/null 2>&1; then
        printf "\nRunning 'openmemory setup'...\n"
        openmemory setup
    else
        printf "\nOnce %s is on your PATH, run:\n  openmemory setup\n" "$INSTALL_DIR"
    fi
}

main
