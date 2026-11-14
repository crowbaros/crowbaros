#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-test ]] || exit 1
install -d /var/lib/crowbaros-lab/gpui-repo
cp -a /home/crowbar/incoming-gpui/repo/. /var/lib/crowbaros-lab/gpui-repo/
restorecon -RF /var/lib/crowbaros-lab/gpui-repo
rpmkeys --import /var/lib/crowbaros-lab/gpui-repo/RPM-GPG-KEY-crowbaros-local
rpmkeys --checksig --verbose /var/lib/crowbaros-lab/gpui-repo/*.rpm
cat > /etc/yum.repos.d/crowbaros-gpui-lab.repo <<'EOF'
[crowbaros-gpui-lab]
name=CrowbarOS GPUI local validation
baseurl=file:///var/lib/crowbaros-lab/gpui-repo
enabled=0
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///var/lib/crowbaros-lab/gpui-repo/RPM-GPG-KEY-crowbaros-local
EOF
# Fedora's signed repositories remain available for declared runtime dependencies.
dnf -y --refresh --enablerepo=crowbaros-gpui-lab install crowbaros-gpui-probe
# DNF install may retain an already-installed version; explicitly promote updates.
dnf -y --enablerepo=crowbaros-gpui-lab upgrade crowbaros-gpui-probe
runuser -u desktop -- crowbaros-gpui-probe --version
