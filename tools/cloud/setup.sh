#!/usr/bin/env bash
# The setup script for a Claude cloud environment (claude.ai/code: the project's or environment's settings, "Setup
# script"). It runs as root before each session starts, and its result is cached while it finishes within a few
# minutes, so it installs only what every session needs: the system libraries the workspace links against, lavapipe
# (software Vulkan) for the render tests, and the pinned Rust toolchain. Paste this file's contents into that field;
# it reads nothing from the repository, because the clone may not exist yet when it runs.
#
# Pair it with these environment variables (the same field set, `.env` format):
#   CARGO_INCREMENTAL=0                     incremental caches roughly double target/ on a 30 GB disk
#   CARGO_PROFILE_DEV_DEBUG=line-tables-only
#   WGPU_BACKEND=vulkan
#   VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
#   VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
#   LP_NUM_THREADS=2
#   GIT_AUTHOR_NAME=Dazsel
#   GIT_AUTHOR_EMAIL=hello@dazsel.com
#   GIT_COMMITTER_NAME=Dazsel
#   GIT_COMMITTER_EMAIL=hello@dazsel.com
#   BASH_DEFAULT_TIMEOUT_MS=600000          cargo builds outlast the two-minute default
#   BASH_MAX_TIMEOUT_MS=3600000
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

# The same packages CI's setup-linux action installs (.github/actions/setup-linux/action.yml).
apt-get update -q
apt-get install -y -q --no-install-recommends \
  pkg-config libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev libx11-dev libxcb1-dev \
  libvulkan1 mesa-vulkan-drivers

# Fail here, with a clear message, rather than in the middle of a render test.
icd=$(find /usr/share/vulkan/icd.d -name 'lvp_icd*.json' | sort | head -n 1)
if [[ -z $icd ]]; then echo "lavapipe's ICD file is missing after installing mesa-vulkan-drivers" >&2; exit 1; fi
echo "lavapipe ICD: $icd (VK_ICD_FILENAMES must name it)"

# The toolchain pinned by rust-toolchain.toml (keep the two in step). rustup also installs it on first use, but doing
# it here puts it in the cached image.
if ! command -v rustup >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
rustup toolchain install 1.99.0 --profile minimal --component rustfmt,clippy --target wasm32-unknown-unknown
rustup default 1.99.0
