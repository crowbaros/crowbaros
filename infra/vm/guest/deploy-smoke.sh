#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-test ]] || exit 1
install -d /var/lib/crowbaros-lab/repo
cp -a /home/crowbar/incoming/repo/. /var/lib/crowbaros-lab/repo/
restorecon -RF /var/lib/crowbaros-lab
rpmkeys --import /var/lib/crowbaros-lab/repo/RPM-GPG-KEY-crowbaros-local
rpmkeys --checksig --verbose /var/lib/crowbaros-lab/repo/*.rpm
cat > /etc/yum.repos.d/crowbaros-local.repo <<'EOF'
[crowbaros-local]
name=CrowbarOS local development only
baseurl=file:///var/lib/crowbaros-lab/repo
enabled=0
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///var/lib/crowbaros-lab/repo/RPM-GPG-KEY-crowbaros-local
EOF
dnf -y --disablerepo='*' --enablerepo=crowbaros-local install crowbaros-build-smoke
runuser -u desktop -- crowbaros-build-smoke
