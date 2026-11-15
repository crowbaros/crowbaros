#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-test ]] || exit 1
install -d /var/lib/crowbaros-lab/studio-repo
cp -a /home/crowbar/incoming-studio/repo/. /var/lib/crowbaros-lab/studio-repo/
restorecon -RF /var/lib/crowbaros-lab/studio-repo
rpmkeys --import /var/lib/crowbaros-lab/studio-repo/RPM-GPG-KEY-crowbaros-local
rpmkeys --checksig --verbose /var/lib/crowbaros-lab/studio-repo/*.rpm
cat > /etc/yum.repos.d/crowbaros-studio-lab.repo <<'EOF'
[crowbaros-studio-lab]
name=CrowbarOS Studio Settings local validation
baseurl=file:///var/lib/crowbaros-lab/studio-repo
enabled=0
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///var/lib/crowbaros-lab/studio-repo/RPM-GPG-KEY-crowbaros-local
EOF
# Fedora's signed repositories remain available for declared runtime dependencies.
dnf -y --refresh --enablerepo=crowbaros-studio-lab install crowbaros-studio-settings
# DNF install may retain an already-installed version; explicitly promote updates.
dnf -y --enablerepo=crowbaros-studio-lab upgrade crowbaros-studio-settings
runuser -u desktop -- crowbaros-studio-settings --version
rpm -V crowbaros-studio-settings
# Wire the packaged login adapter into this lab-owned compositor configuration.
# Do not edit the user's Hyprland files or reload an unrelated desktop session.
python3 - <<'PY'
from pathlib import Path
p = Path('/etc/crowbaros-lab/hyprland.lua')
s = p.read_text()
if '/usr/share/crowbaros-studio-settings/hyprland.lua' not in s:
    s += '''\n-- Optional packaged Studio Settings session integration.
local settings_hook = "/usr/share/crowbaros-studio-settings/hyprland.lua"
local settings_file = io.open(settings_hook, "r")
if settings_file then settings_file:close(); dofile(settings_hook) end
'''
    p.write_text(s)
PY
restorecon /etc/crowbaros-lab/hyprland.lua
