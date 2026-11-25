#!/usr/bin/env bash
set -euo pipefail

# Lightweight background daemon manager for the Rusty Intelli Scanner HTML Report Server.
# Reads `report_server.listen_addr` and `reports.output_dir` directly from `scanner_config.yaml`!

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
CONFIG_FILE="${REPO_DIR}/scanner_config.yaml"
BIN="${REPO_DIR}/target/release/rusty_scanner"
PID_FILE="${REPO_DIR}/.report_server.pid"
LOG_FILE="${REPO_DIR}/reports/report_server.log"

ACTION="${1:-status}"

is_running() {
  if [[ -f "${PID_FILE}" ]]; then
    local pid
    pid="$(cat "${PID_FILE}" 2>/dev/null || true)"
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      return 0
    fi
  fi
  return 1
}

case "${ACTION}" in
  start)
    if is_running; then
      echo "Report server is already running (PID $(cat "${PID_FILE}"))."
      exit 0
    fi
    mkdir -p "${REPO_DIR}/reports"
    if [[ -x "${BIN}" ]]; then
      nohup "${BIN}" -c "${CONFIG_FILE}" --serve-reports >"${LOG_FILE}" 2>&1 &
      echo $! >"${PID_FILE}"
      sleep 0.3
      cat "${LOG_FILE}" || true
      echo "Started rusty_scanner --serve-reports in background (PID $(cat "${PID_FILE}"), log: ${LOG_FILE})"
    else
      echo "Binary ${BIN} not found; falling back to python3 -m http.server on 0.0.0.0:8787..."
      nohup python3 -m http.server 8787 --bind 0.0.0.0 --directory "${REPO_DIR}/reports" >"${LOG_FILE}" 2>&1 &
      echo $! >"${PID_FILE}"
      echo "Started python3 http.server on http://0.0.0.0:8787/ (PID $(cat "${PID_FILE}"))"
    fi
    ;;
  stop)
    if is_running; then
      kill "$(cat "${PID_FILE}")" && rm -f "${PID_FILE}"
      echo "Stopped report server."
    else
      rm -f "${PID_FILE}"
      echo "Report server is not running."
    fi
    ;;
  restart)
    "$0" stop
    "$0" start
    ;;
  status)
    if is_running; then
      echo "Report server is RUNNING (PID $(cat "${PID_FILE}"))."
      [[ -f "${LOG_FILE}" ]] && head -n 8 "${LOG_FILE}" || true
    else
      echo "Report server is STOPPED."
    fi
    ;;
  *)
    echo "Usage: $0 {start|stop|restart|status}"
    exit 1
    ;;
esac
