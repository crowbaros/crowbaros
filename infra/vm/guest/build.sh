#!/usr/bin/env bash
# Infrastructure provisioning; run only inside the Fedora build VM.
set -euo pipefail
source /etc/os-release
[[ $ID == fedora && $VERSION_ID == 45 && $(hostname) == crowbaros-build ]] || exit 1
dnf -y install mock mock-core-configs rpm-build rpmdevtools rpm-sign createrepo_c \
  git gcc gcc-c++ clang cmake ninja-build make pkgconf-pkg-config gdb \
  rust cargo rustfmt clippy openssl-devel fontconfig-devel freetype-devel \
  libxkbcommon-devel libxkbcommon-x11-devel wayland-devel wayland-protocols-devel libxcb-devel \
  mesa-libEGL-devel mesa-libGL-devel vulkan-loader-devel alsa-lib-devel \
  libzstd-devel gnupg2 rsync jq tar unzip firewalld
usermod -aG mock crowbar
systemctl enable --now firewalld
firewall-cmd --permanent --add-service=ssh
firewall-cmd --reload
install -d -o crowbar -g crowbar /home/crowbar/src /home/crowbar/artifacts
install -d /var/lib/crowbaros-lab
date -u +%FT%TZ > /var/lib/crowbaros-lab/provisioned
