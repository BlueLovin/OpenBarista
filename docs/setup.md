---
layout: default
title: Toolchain Setup
nav_order: 4
---

# Toolchain Setup

OpenBarista is built with the Espressif Rust toolchain targeting the ESP32's Xtensa architecture. This page walks through getting your development environment ready.

---

## Prerequisites

You need these installed on your system first:

- **Git**
- **Python 3** (for ESP-IDF build system)
- **cmake** and **ninja-build**
- **Standard C build tools** (gcc/clang, make)

### Debian / Ubuntu

```sh
sudo apt update
sudo apt install -y git python3 python3-venv python3-pip cmake ninja-build \
  build-essential pkg-config libssl-dev libudev-dev
```

### macOS

```sh
xargs brew install < <(echo "cmake ninja python3")
xcode-select --install   # if not already done
```

### Arch Linux

```sh
sudo pacman -S git python cmake ninja base-devel openssl
```

---

## Automated Bootstrap

The easiest way to set everything up is the included bootstrap script:

```sh
git clone https://github.com/BlueLovin/OpenBarista.git
cd OpenBarista
bash scripts/bootstrap.sh
```

**What bootstrap does:**

1. Ensures `~/.cargo/bin` is on your `PATH`
2. Installs the host Rust toolchain (`stable-<host-triple>`) for desktop tooling
3. Installs [`espup`](https://github.com/esp-rs/espup) if missing
4. Installs the Espressif Rust toolchain (named `esp`)
5. Generates `.esp/export-esp.sh`
6. Installs `ldproxy`, `espflash`, and `cargo-espflash`

After bootstrap completes, source the environment:

```sh
source .esp/export-esp.sh
```

---

## Manual Setup

If you prefer to install things yourself:

### 1. Install Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### 2. Install espup and the ESP toolchain

```sh
cargo install espup
espup install
```

This installs the `esp` toolchain with Xtensa LLVM support.

### 3. Source the ESP environment

```sh
source ~/.espup/export-esp.sh
# or wherever espup placed its export script
```

### 4. Install flashing tools

```sh
cargo install ldproxy espflash cargo-espflash
```

---

## Verify Your Setup

Check that the toolchain is available:

```sh
rustup toolchain list | grep esp
# should show: esp

rustup run esp rustc --version
# should show the esp-flavored rustc
```

Check that espflash is installed:

```sh
espflash --version
```

---

## Toolchain Details

The project pins its toolchain in `rust-toolchain.toml`:

```toml
[toolchain]
channel = "esp"
```

The build target is configured in `.cargo/config.toml`:

- **Target:** `xtensa-esp32-espidf`
- **Linker:** `ldproxy`
- **Runner:** `espflash flash --monitor`

You shouldn't need to specify these manually — Cargo picks them up automatically.

---

## Troubleshooting

**`error: toolchain 'esp' is not installed`**
Run `espup install` again and make sure you've sourced the export script.

**`ldproxy: command not found`**
Run `cargo install ldproxy` and ensure `~/.cargo/bin` is on your `PATH`.

**`espflash: permission denied` on `/dev/ttyUSB0`**
Add your user to the `dialout` group:
```sh
sudo usermod -aG dialout $USER
# then log out and back in
```

**Python/cmake errors during build**
ESP-IDF's build system needs Python 3 and cmake. Make sure both are installed and accessible.
