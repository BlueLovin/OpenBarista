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

require_python_venv() {
  local tmpdir
  tmpdir="$(mktemp -d)"

  if ! python3 -m venv "${tmpdir}/probe" >/dev/null 2>&1; then
    rm -rf "${tmpdir}"
    echo "python3 can run, but python3 -m venv is not available." >&2
    echo "Install your distro's venv package before building ESP-IDF projects." >&2
    echo "On Debian/Ubuntu, use: sudo apt install python3-venv" >&2
    echo "If your distro splits it by Python version, install the matching package" >&2
    echo "(for example: python3.13-venv)." >&2
    exit 1
  fi

  rm -rf "${tmpdir}"
}

has_usable_libclang() {
  local libdir="$1"
  local candidate

  [[ -d "${libdir}" ]] || return 1

  shopt -s nullglob
  for candidate in \
    "${libdir}"/libclang.so \
    "${libdir}"/libclang.so.* \
    "${libdir}"/libclang-*.so \
    "${libdir}"/libclang-*.so.*
  do
    if [[ -e "${candidate}" ]]; then
      shopt -u nullglob
      return 0
    fi
  done
  shopt -u nullglob

  return 1
}

require_generated_libclang() {
  local libclang_path
  libclang_path="$(sed -n 's/^export LIBCLANG_PATH="\([^"]*\)"$/\1/p' "${ESP_ENV_FILE}" | head -n 1)"

  if [[ -z "${libclang_path}" ]] || ! has_usable_libclang "${libclang_path}"; then
    echo "espup did not generate a usable LIBCLANG_PATH in ${ESP_ENV_FILE}." >&2
    echo "Rerun bootstrap after updating espup, or run:" >&2
    echo "  espup update --name esp --targets esp32 --std --extended-llvm --export-file ${ESP_ENV_FILE}" >&2
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
require_python_venv

HOST_TRIPLE="$(rustc +stable -vV | sed -n 's/^host: //p')"
if [[ -n "${HOST_TRIPLE}" ]]; then
  rustup toolchain install "stable-${HOST_TRIPLE}"
fi

if ! command -v espup >/dev/null 2>&1; then
  cargo +stable install espup --locked
fi

espup install \
  --name esp \
  --targets esp32 \
  --extended-llvm \
  --std \
  --export-file "${ESP_ENV_FILE}"

require_generated_libclang

cargo +stable install --locked ldproxy espflash cargo-espflash

cat <<EOF

Bootstrap complete.

Open a new shell, or run:
source "${SHELL_RC}"

Then build and flash with:
source "${ESP_ENV_FILE}"
cargo run

The ESP environment file for this project is:
${ESP_ENV_FILE}
EOF
