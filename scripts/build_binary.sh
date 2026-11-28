#!/usr/bin/env bash
set -euo pipefail

# Convenience top-level wrapper for `scripts/build_binary.sh`.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${SCRIPT_DIR}/scripts/build_binary.sh" "$@"
