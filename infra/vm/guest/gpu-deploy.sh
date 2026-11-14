#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-test ]] || exit 1
install -d /var/lib/crowbaros-lab/gpu-repo
cp -a /home/crowbar/incoming-gpu/repo/. /var/lib/crowbaros-lab/gpu-repo/
restorecon -RF /var/lib/crowbaros-lab/gpu-repo
rpmkeys --import /var/lib/crowbaros-lab/gpu-repo/RPM-GPG-KEY-crowbaros-local
rpmkeys --checksig --verbose /var/lib/crowbaros-lab/gpu-repo/*.rpm
cat > /etc/yum.repos.d/crowbaros-gpu-lab.repo <<'EOF'
[crowbaros-gpu-lab]
name=CrowbarOS isolated GPU validation
baseurl=file:///var/lib/crowbaros-lab/gpu-repo
enabled=0
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///var/lib/crowbaros-lab/gpu-repo/RPM-GPG-KEY-crowbaros-local
EOF
dnf -y --refresh --enablerepo=crowbaros-gpu-lab install crowbaros-mesa-intel-virtio
dnf -y --enablerepo=crowbaros-gpu-lab upgrade crowbaros-mesa-intel-virtio
rpm -V crowbaros-mesa-intel-virtio
printf 'Driver installed under /opt/crowbaros-lab/mesa-intel-virtio; select explicitly for validation.\n'
