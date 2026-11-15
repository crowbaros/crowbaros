#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-build && $(id -un) == crowbar ]] || exit 1
cd "$HOME/src/lab"
mkdir -p "$HOME/artifacts/smoke" "$HOME/.local/share/crowbaros-dev-signing"
chmod 700 "$HOME/.local/share/crowbaros-dev-signing"
export GNUPGHOME="$HOME/.local/share/crowbaros-dev-signing"
signer='CrowbarOS local development (NOT FOR RELEASE) <local-build@crowbaros.invalid>'
if ! gpg --batch --list-secret-keys "$signer" >/dev/null 2>&1; then
  gpg --batch --pinentry-mode loopback --passphrase '' --quick-generate-key "$signer" rsa3072 sign 1y
fi
fingerprint=$(gpg --batch --with-colons --list-secret-keys "$signer" | awk -F: '$1=="fpr" {print $10; exit}')
mock -r fedora-45-x86_64 --buildsrpm --spec fixtures/crowbaros-build-smoke.spec \
  --sources fixtures --resultdir "$HOME/artifacts/smoke/srpm"
mock -r fedora-45-x86_64 --rebuild "$HOME"/artifacts/smoke/srpm/*.src.rpm \
  --resultdir "$HOME/artifacts/smoke/rpms"
mkdir -p "$HOME/artifacts/smoke/repo"
find "$HOME/artifacts/smoke/rpms" -maxdepth 1 -name '*.rpm' ! -name '*.src.rpm' \
  -exec cp -t "$HOME/artifacts/smoke/repo" {} +
rpmsign --define '_openpgp_sign gpg' --define "_openpgp_sign_id $fingerprint" \
  --define "_gpg_name $fingerprint" --define "_gpg_path $GNUPGHOME" \
  --addsign "$HOME"/artifacts/smoke/repo/*.rpm
gpg --armor --export "$fingerprint" > "$HOME/artifacts/smoke/repo/RPM-GPG-KEY-crowbaros-local"
createrepo_c "$HOME/artifacts/smoke/repo"
gpg --batch --yes --armor --detach-sign "$HOME/artifacts/smoke/repo/repodata/repomd.xml"
printf '%s\n' "$fingerprint" > "$HOME/artifacts/smoke/repo/signing-fingerprint.txt"
tar -C "$HOME/artifacts/smoke" -czf "$HOME/artifacts/smoke-repo.tar.gz" repo
