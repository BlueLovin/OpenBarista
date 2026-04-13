#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ESP_ENV_DIR="${ROOT_DIR}/.esp"
ESP_ENV_FILE="${ESP_ENV_DIR}/export-esp.sh"
SHELL_RC="${HOME}/.zshrc"

if [[ ! -f "${SHELL_RC}" ]] && [[ -f "${HOME}/.bashrc" ]]; then
  SHELL_RC="${HOME}/.bashrc"
fi

ensure_path_line() {
  local line='export PATH="$HOME/.cargo/bin:$PATH"'

  if [[ -f "${SHELL_RC}" ]] && ! grep -Fq 'export PATH="$HOME/.cargo/bin:$PATH"' "${SHELL_RC}"; then
    printf '\n%s\n' "${line}" >> "${SHELL_RC}"
  fi

  export PATH="${HOME}/.cargo/bin:${PATH}"
}

require_command() {
  local cmd="$1"
  if ! command -v "${cmd}" >/dev/null 2>&1; then
    echo "Missing required command: ${cmd}" >&2
    echo "Install your system build dependencies first, then rerun this script." >&2
    exit 1
  fi
}

ensure_path_line

mkdir -p "${ESP_ENV_DIR}"

require_command cargo
require_command rustup
require_command rustc
require_command git
require_command python3
require_command cmake

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
if [[ -n "${HOST_TRIPLE}" ]]; then
  rustup toolchain install "stable-${HOST_TRIPLE}"
fi

if ! command -v espup >/dev/null 2>&1; then
  cargo +stable install espup --locked
fi

espup install \
  --name esp \
  --targets esp32 \
  --std \
  --export-file "${ESP_ENV_FILE}"

cargo install --locked ldproxy espflash cargo-espflash

cat <<EOF

Bootstrap complete.

Open a new shell, or run:
source "${SHELL_RC}"

Then build and flash with:
bash "${ROOT_DIR}/scripts/flash.sh"

The ESP environment file for this project is:
${ESP_ENV_FILE}
EOF
