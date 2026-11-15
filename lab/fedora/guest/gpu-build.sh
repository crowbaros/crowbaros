#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-build && $(id -un) == crowbar ]] || exit 1
artifact="$HOME/artifacts/gpu"
mkdir -p "$artifact/sources"
source="$artifact/sources/mesa-26.2.6.tar.xz"
if [[ ! -f $source ]]; then
  curl --fail --location --retry 3 https://archive.mesa3d.org/mesa-26.2.6.tar.xz -o "$source"
fi
printf '%s  %s\n' b9436b3e1a8be76fbd4823dc884f90596ab7c6ac5e1867a305e66ffccfb31a92 "$source" | sha256sum --check
mock -r fedora-45-x86_64 --buildsrpm \
  --spec "$HOME/src/gpu/gpu/crowbaros-mesa-intel-virtio.spec" \
  --sources "$artifact/sources" --resultdir "$artifact/srpm"
mock -r fedora-45-x86_64 --rebuild \
  "$artifact/srpm/crowbaros-mesa-intel-virtio-26.2.6-1.fc45.src.rpm" \
  --resultdir "$artifact/rpms"
export GNUPGHOME="$HOME/.local/share/crowbaros-dev-signing"
signer='CrowbarOS local development (NOT FOR RELEASE) <local-build@crowbaros.invalid>'
fingerprint=$(gpg --batch --with-colons --list-secret-keys "$signer" | awk -F: '$1=="fpr" {print $10; exit}')
test -n "$fingerprint"
mkdir -p "$artifact/repo"
cp "$artifact/rpms/crowbaros-mesa-intel-virtio-26.2.6-1.fc45.x86_64.rpm" "$artifact/repo/"
rpmsign --define '_openpgp_sign gpg' --define "_openpgp_sign_id $fingerprint" \
  --define "_gpg_name $fingerprint" --define "_gpg_path $GNUPGHOME" --addsign "$artifact/repo/"*.rpm
gpg --armor --export "$fingerprint" > "$artifact/repo/RPM-GPG-KEY-crowbaros-local"
createrepo_c "$artifact/repo"
gpg --batch --yes --armor --detach-sign "$artifact/repo/repodata/repomd.xml"
tar -C "$artifact" -czf "$HOME/artifacts/gpu-repo.tar.gz" repo
sha256sum "$artifact/repo/"*.rpm
