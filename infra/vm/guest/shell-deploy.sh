#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-test ]] || exit 1
install -d /var/lib/crowbaros-lab/shell-repo
cp -a /home/crowbar/incoming-shell/repo/. /var/lib/crowbaros-lab/shell-repo/
restorecon -RF /var/lib/crowbaros-lab/shell-repo
rpmkeys --import /var/lib/crowbaros-lab/shell-repo/RPM-GPG-KEY-crowbaros-local
rpmkeys --checksig --verbose /var/lib/crowbaros-lab/shell-repo/*.rpm
cat > /etc/yum.repos.d/crowbaros-shell-lab.repo <<'EOF'
[crowbaros-shell-lab]
name=CrowbarOS Desktop shell local validation
baseurl=file:///var/lib/crowbaros-lab/shell-repo
enabled=0
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///var/lib/crowbaros-lab/shell-repo/RPM-GPG-KEY-crowbaros-local
EOF
# Fedora's signed repositories remain available for declared runtime dependencies.
dnf -y --refresh --enablerepo=crowbaros-shell-lab install crowbaros-shell
# DNF install may retain an already-installed version; explicitly promote updates.
dnf -y --enablerepo=crowbaros-shell-lab upgrade crowbaros-shell
runuser -u desktop -- crowbaros-shell --version
rpm -V crowbaros-shell
# This hook belongs to the disposable lab, never the host desktop.
python3 - <<'PYCODE'
from pathlib import Path
p=Path('/etc/crowbaros-lab/hyprland.lua')
s=p.read_text()
s=s.replace('hl.on("hyprland.start", function () hl.exec_cmd("foot") end)', '-- Foot remains available with Alt+Return; the shell owns desktop startup.')
if '/usr/share/crowbaros-shell/hyprland.lua' not in s:
 s+='\nlocal shell_hook="/usr/share/crowbaros-shell/hyprland.lua"\nlocal shell_file=io.open(shell_hook,"r")\nif shell_file then shell_file:close(); dofile(shell_hook) end\n'
if '-- CrowbarOS lab mouse controls.' not in s:
 s += '\n-- CrowbarOS lab mouse controls.\nhl.config({general={resize_on_border=true}})\nhl.bind("ALT + mouse:272", hl.dsp.window.drag(), {mouse=true})\nhl.bind("ALT + mouse:273", hl.dsp.window.resize(), {mouse=true})\n'
p.write_text(s)
PYCODE
restorecon /etc/crowbaros-lab/hyprland.lua
