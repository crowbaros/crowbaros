#!/usr/bin/env bash
# Build automation, executed as crowbar in the Fedora builder.
set -euo pipefail
[[ $(hostname) == crowbaros-build && $(id -un) == crowbar ]] || exit 1
mkdir -p "$HOME/artifacts/shell"
exec > >(tee "$HOME/artifacts/shell/build-session.log") 2>&1
cd "$HOME/src/shell"
python3 packaging/prepare-gpui.py
cargo fmt --check
test -f Cargo.lock
mkdir -p .cargo "$HOME/artifacts/shell/sources"
cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml 2> "$HOME/artifacts/shell/vendor.log"
test -s .cargo/config.toml
cargo metadata --locked --offline --format-version 1 > "$HOME/artifacts/shell/dependency-graph.json"
python3 - <<'PY'
import json, pathlib, re, shutil
root = pathlib.Path.cwd()
graph = json.loads((pathlib.Path.home() / 'artifacts/shell/dependency-graph.json').read_text())
records, expressions = [], {'MIT', 'Apache-2.0', 'OFL-1.1'}
licenses = root / 'dependency-licenses'
licenses.mkdir(exist_ok=True)
for package in graph['packages']:
    if package['name'] == 'crowbaros-shell':
        continue
    expression = package['license']
    if not expression or not re.fullmatch(r'[A-Za-z0-9 .:+()/-]+', expression):
        raise SystemExit(f"Review dependency license: {package['name']} {expression!r}")
    expressions.add(expression.replace('/', ' OR '))
    crate = pathlib.Path(package['manifest_path']).parent
    destination = licenses / f"{package['name']}-{package['version']}"
    destination.mkdir(exist_ok=True)
    for entry in crate.rglob('*'):
        if entry.is_file() and entry.name.lower().startswith(('license', 'licence', 'copying', 'copyright', 'notice')):
            target = destination / entry.relative_to(crate)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(entry, target)
    records.append({key: package.get(key) for key in ('name', 'version', 'license', 'source', 'repository')})
(root / 'THIRD_PARTY.json').write_text(json.dumps(sorted(records, key=lambda p:(p['name'],p['version'])), indent=2) + '\n')
template = (root / 'packaging/crowbaros-shell.spec.in').read_text()
expression = ' AND '.join('(' + value + ')' for value in sorted(expressions))
(pathlib.Path.home() / 'artifacts/shell/crowbaros-shell.spec').write_text(template.replace('@LICENSE_EXPRESSION@', expression))
PY
{
  printf 'GPUI: 0.2.2\nBackend: Wayland (X11 feature disabled)\n'
  rustc --version
  cargo --version
  sha256sum Cargo.lock src/*.rs fonts/*.ttf patches/*.patch assets/*
} > BUILD-MANIFEST.txt
tar --exclude=./target --exclude=./.git --exclude=./.upstream-cache --transform='s,^\.,crowbaros-shell-0.1.1,' \
  -czf "$HOME/artifacts/shell/sources/crowbaros-shell-0.1.1.tar.gz" .
mock -r fedora-45-x86_64 --buildsrpm --spec "$HOME/artifacts/shell/crowbaros-shell.spec" \
  --sources "$HOME/artifacts/shell/sources" --resultdir "$HOME/artifacts/shell/srpm"
mock -r fedora-45-x86_64 --no-cleanup-after --rebuild "$HOME/artifacts/shell/srpm/crowbaros-shell-0.1.1-1.fc45.src.rpm" \
  --resultdir "$HOME/artifacts/shell/rpms"

export GNUPGHOME="$HOME/.local/share/crowbaros-dev-signing"
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
signer='CrowbarOS local development (NOT FOR RELEASE) <local-build@crowbaros.invalid>'
if ! gpg --batch --list-secret-keys "$signer" >/dev/null 2>&1; then
  gpg --batch --pinentry-mode loopback --passphrase '' --quick-generate-key "$signer" rsa3072 sign 1y
fi
fingerprint=$(gpg --batch --with-colons --list-secret-keys "$signer" | awk -F: '$1=="fpr" {print $10; exit}')
mkdir -p "$HOME/artifacts/shell/repo"
cp "$HOME/artifacts/shell/rpms/crowbaros-shell-0.1.1-1.fc45.x86_64.rpm" "$HOME/artifacts/shell/repo/"
rpmsign --define '_openpgp_sign gpg' --define "_openpgp_sign_id $fingerprint" \
  --define "_gpg_name $fingerprint" --define "_gpg_path $GNUPGHOME" \
  --addsign "$HOME/artifacts/shell/repo/"*.rpm
gpg --armor --export "$fingerprint" > "$HOME/artifacts/shell/repo/RPM-GPG-KEY-crowbaros-local"
createrepo_c "$HOME/artifacts/shell/repo"
gpg --batch --yes --armor --detach-sign "$HOME/artifacts/shell/repo/repodata/repomd.xml"
cp Cargo.lock BUILD-MANIFEST.txt "$HOME/artifacts/shell/"
tar -C "$HOME/artifacts/shell" -czf "$HOME/artifacts/shell-repo.tar.gz" repo
sha256sum "$HOME/artifacts/shell/repo/"*.rpm
