#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ESP_ENV_FILE="${ROOT_DIR}/.esp/export-esp.sh"

if [[ ! -f "${ESP_ENV_FILE}" ]]; then
  echo "Missing ${ESP_ENV_FILE}. Run bash scripts/bootstrap.sh first." >&2
  exit 1
fi

source "${ESP_ENV_FILE}"

cd "${ROOT_DIR}"
cargo build