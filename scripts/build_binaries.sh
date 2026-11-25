#!/usr/bin/env bash
set -euo pipefail

# Cross-platform release binary builder for `rusty_scanner`.
#
# Usage:
#   ./scripts/build_binaries.sh <platform> <arch>
#   ./scripts/build_binaries.sh linux-windows
#   ./scripts/build_binaries.sh all

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${REPO_ROOT}/dist"

export PATH="/opt/homebrew/bin:/usr/local/bin:${HOME}/.cargo/bin:${PATH}"

usage() {
    cat <<EOF
Usage:
  $0 <platform> <arch>
  $0 linux-windows
  $0 all

Platforms & Architectures:
  macos   arm64 | x86_64
  linux   x86_64 | arm64
  windows x86_64 | arm64
EOF
    exit 1
}

if [[ $# -lt 1 ]]; then
    usage
fi

resolve_target() {
    local platform="$1"
    local arch="$2"
    case "${platform}:${arch}" in
        macos:arm64|macos:aarch64)
            echo "aarch64-apple-darwin|aarch64-apple-darwin||native"
            ;;
        macos:x86_64|macos:x64|macos:amd64)
            echo "x86_64-apple-darwin|x86_64-apple-darwin||native"
            ;;
        linux:x86_64|linux:x64|linux:amd64)
            echo "x86_64-unknown-linux-gnu|x86_64-unknown-linux-gnu.2.31||zigbuild"
            ;;
        linux:arm64|linux:aarch64)
            echo "aarch64-unknown-linux-gnu|aarch64-unknown-linux-gnu.2.31||zigbuild"
            ;;
        windows:x86_64|windows:x64|windows:amd64)
            echo "x86_64-pc-windows-gnu|x86_64-pc-windows-gnu|.exe|zigbuild"
            ;;
        windows:arm64|windows:aarch64)
            echo "aarch64-pc-windows-gnullvm|aarch64-pc-windows-gnullvm|.exe|zigbuild"
            ;;
        *)
            echo "ERROR: Unsupported platform/arch combination: ${platform} ${arch}" >&2
            usage
            ;;
    esac
}

normalize_arch() {
    case "$1" in
        aarch64|arm64) echo "arm64" ;;
        x86_64|x64|amd64) echo "x86_64" ;;
        *) echo "$1" ;;
    esac
}

build_one() {
    local platform="$1"
    local arch
    arch="$(normalize_arch "$2")"

    local spec
    spec="$(resolve_target "${platform}" "${arch}")"
    IFS='|' read -r rust_triple build_target ext builder <<< "${spec}"

    if [[ "${builder}" == "zigbuild" ]]; then
        if ! command -v cargo-zigbuild >/dev/null 2>&1; then
            echo "ERROR: cargo-zigbuild not found in PATH." >&2
            exit 1
        fi
    fi

    echo "================================================================"
    echo "[build_binaries] Building rusty_scanner & rusty_triage: ${platform}/${arch} (target=${build_target})"
    echo "================================================================"

    mkdir -p "${DIST_DIR}"
    cd "${REPO_ROOT}"

    if [[ "${builder}" == "native" ]]; then
        cargo build --release --offline --target "${build_target}"
    else
        cargo zigbuild --release --offline --target "${build_target}"
    fi

    for bin_name in rusty_scanner rusty_triage; do
        local src_bin="${REPO_ROOT}/target/${rust_triple}/release/${bin_name}${ext}"
        local dst_bin="${DIST_DIR}/${bin_name}-${platform}-${arch}${ext}"
        cp -f "${src_bin}" "${dst_bin}"
        if [[ -z "${ext}" ]]; then
            chmod +x "${dst_bin}"
        fi
        echo "[build_binaries] -> ${dst_bin}"
    done
}

case "$1" in
    -h|--help)
        usage
        ;;
    all)
        build_one macos   arm64
        build_one macos   x86_64
        build_one linux   x86_64
        build_one linux   arm64
        build_one windows x86_64
        build_one windows arm64
        ;;
    linux-windows)
        build_one linux   x86_64
        build_one linux   arm64
        build_one windows x86_64
        build_one windows arm64
        ;;
    *)
        if [[ $# -lt 2 ]]; then
            usage
        fi
        build_one "$1" "$2"
        ;;
esac

if ls "${DIST_DIR}"/rusty_* >/dev/null 2>&1; then
    (
        cd "${DIST_DIR}"
        shasum -a 256 rusty_* > SHA256SUMS.txt
    )
    echo "[build_binaries] Updated ${DIST_DIR}/SHA256SUMS.txt"
fi
