#!/usr/bin/env bash
# Infrastructure provisioning; run only inside the Fedora test VM.
set -euo pipefail
source /etc/os-release
[[ $ID == fedora && $VERSION_ID == 45 && $(hostname) == crowbaros-test ]] || exit 1
# Upstream recommends this COPR for Fedora. Keep it disabled by default and
# restricted to Hyprland's package family; replace it with our own RPMs later.
curl -fsSL --retry 3 https://download.copr.fedorainfracloud.org/results/lionheartp/Hyprland/pubkey.gpg \
  -o /etc/pki/rpm-gpg/RPM-GPG-KEY-hyprland-lab
printf '%s\n' '9f47c74eec4fea0f5a8cee1c6532bbeb33eee8134f762314bf16815a720dc047  /etc/pki/rpm-gpg/RPM-GPG-KEY-hyprland-lab' | sha256sum -c -
rpmkeys --import /etc/pki/rpm-gpg/RPM-GPG-KEY-hyprland-lab
cat > /etc/yum.repos.d/crowbaros-hyprland-baseline.repo <<'EOF'
[crowbaros-hyprland-baseline]
name=Temporary Hyprland lab baseline (lionheartp COPR)
baseurl=https://download.copr.fedorainfracloud.org/results/lionheartp/Hyprland/fedora-45-x86_64/
enabled=0
gpgcheck=1
repo_gpgcheck=0
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-hyprland-lab
skip_if_unavailable=False
includepkgs=hypr* aquamarine* xdg-desktop-portal-hyprland*
EOF
dnf -y --enablerepo=crowbaros-hyprland-baseline install hyprland foot greetd xorg-x11-server-Xwayland \
  xdg-desktop-portal-hyprland xdg-desktop-portal-gtk \
  pipewire pipewire-pulseaudio wireplumber mesa-dri-drivers mesa-vulkan-drivers \
  vulkan-tools glx-utils wayland-utils dbus-tools grim \
  google-noto-sans-fonts firewalld rsync jq
id desktop >/dev/null 2>&1 || useradd -m -s /bin/bash desktop
# This ordinary user is deliberately NOT in wheel and has no SSH key.
# The local-only VM console autologin is a development convenience, not an OS default.
install -d -m 0755 /etc/crowbaros-lab /var/lib/crowbaros-lab
cat > /etc/crowbaros-lab/hyprland.lua <<'EOF'
-- Disposable integration lab; this is not CrowbarOS's desktop configuration.
hl.monitor({ output = "", mode = "1280x800@60", position = "auto", scale = 1 })
hl.config({
    misc = { disable_hyprland_logo = true, disable_splash_rendering = true },
    animations = { enabled = false },
    decoration = { blur = { enabled = false } },
})
-- Alt avoids collisions with Omarchy's Super shortcuts outside the VM.
hl.bind("ALT + Return", hl.dsp.exec_cmd("foot"))
hl.bind("ALT + Q", hl.dsp.window.close())
hl.bind("ALT + V", hl.dsp.window.float({ action = "toggle" }))
hl.on("hyprland.start", function () hl.exec_cmd("foot") end)
EOF
cat > /etc/greetd/config.toml <<'EOF'
[terminal]
vt = 1
[default_session]
command = "/usr/bin/agreety --cmd '/usr/bin/start-hyprland -- --config /etc/crowbaros-lab/hyprland.lua'"
user = "greetd"
[initial_session]
command = "/usr/bin/start-hyprland -- --config /etc/crowbaros-lab/hyprland.lua"
user = "desktop"
EOF
systemctl enable --now firewalld
firewall-cmd --permanent --add-service=ssh
firewall-cmd --reload
systemctl set-default graphical.target
systemctl enable greetd
date -u +%FT%TZ > /var/lib/crowbaros-lab/provisioned
