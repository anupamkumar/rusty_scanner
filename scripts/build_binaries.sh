#!/usr/bin/env bash
set -euo pipefail

# Alias forwarding to `scripts/build_binary.sh`.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${SCRIPT_DIR}/build_binary.sh" "$@"
