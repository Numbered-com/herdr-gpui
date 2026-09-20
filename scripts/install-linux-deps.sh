#!/usr/bin/env bash
# Ubuntu 24.04, native x86_64 or ARM64. GPUI 0.2.2 defaults enable X11 and Wayland.
set -euo pipefail
sudo apt-get update
sudo apt-get install --no-install-recommends -y \
  build-essential pkg-config libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev \
  libfreetype6-dev libfontconfig1-dev fonts-dejavu-core
