#!/usr/bin/env bash
# Publishes the example module to real OCI registries and uses it from a
# project, the way the documentation says to.
#
# It expects an open registry on localhost:5000 and one that wants the login
# reader / s3cret on localhost:5001, and `oras` on the PATH, which is used to
# show that a module published by another tool installs too. The modules
# workflow starts both; see .github/workflows/modules.yml.
#
#   tools/real_registry_test.sh /path/to/meridian
set -euo pipefail

meridian=${1:?usage: real_registry_test.sh /path/to/meridian}
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

layer=application/vnd.meridian.module.layer.v1.tar+gzip
artifact=application/vnd.meridian.module.v1

say() { printf '\n== %s\n' "$*"; }

fail() {
  echo "FAILED: $*" >&2
  exit 1
}

# Copies the example module, as the given version, and packs it.
prepare() {
  local version=$1
  local src="$work/src-$version"
  cp -r "$root/examples/modules/hardening" "$src"
  sed -i "s/^  version: .*/  version: $version/" "$src/meridian.yaml"
  (cd "$src" && "$meridian" module pack --out "$work/module-$version.tar.gz")
}

# Publishes a version with `meridian module push`.
publish() {
  local repository=$1 version=$2
  prepare "$version"
  (cd "$work/src-$version" && "$meridian" module push "$repository" --insecure)
}

# Publishes a version with oras, as the documentation says another tool can.
publish_with_oras() {
  local repository=$1 version=$2
  prepare "$version"
  (cd "$work" && oras push --plain-http "$repository:$version" \
    --artifact-type "$artifact" "module-$version.tar.gz:$layer")
}

# Writes a project that requires the module from a repository.
project() {
  local dir=$1 repository=$2 requirement=$3
  mkdir -p "$dir"
  cp "$root/examples/modules/site/web.yaml" "$root/examples/modules/site/data.yaml" "$dir/"
  cat > "$dir/meridian.yaml" <<YAML
apiVersion: meridian/v1
kind: Module
metadata:
  name: registry-test
spec:
  modules:
    - name: acme/hardening
      version: "$requirement"
      oci: $repository
      insecure: true
YAML
}

say "an open registry"
open=localhost:5000/acme/meridian-hardening
publish_with_oras "$open" 1.2.0
publish "$open" 1.3.0
publish "$open" 2.0.0

# A version is published once: the same content again is a no-op, and other
# content under the same version is refused.
(cd "$work/src-1.3.0" && "$meridian" module push "$open" --insecure) | tee "$work/again.txt"
grep -q 'is already published' "$work/again.txt" || fail "a second push of the same content was not recognised"
cp -r "$work/src-1.3.0" "$work/src-changed"
echo '# changed after release' >> "$work/src-changed/documents/ssh.yaml"
if (cd "$work/src-changed" && "$meridian" module push "$open" --insecure) 2> "$work/replace.txt"; then
  fail "a published version was replaced"
fi
grep -q 'a version is published once' "$work/replace.txt" || { cat "$work/replace.txt"; fail "the refusal does not say why"; }
# A tag that is not a version, and an image that is not a module, are both
# in the repository and must both be ignored.
(cd "$work" && echo notes > notes.txt && oras push --plain-http "$open:latest" notes.txt:text/plain)

project "$work/site" "$open" "^1.2"
cd "$work/site"
"$meridian" module install | tee "$work/install.txt"
grep -q '^+ acme/hardening 1.3.0 ' "$work/install.txt" || fail "the newest 1.x was not chosen"
grep -q 'resolved: sha256:' meridian.lock || fail "the manifest digest was not pinned"
"$meridian" module verify
"$meridian" module install --locked

# What was fetched is what was packed.
packed=$(cd "$work/src-1.3.0" && "$meridian" module pack --out "$work/again.tar.gz" | sed -n 's/^  integrity:  //p')
grep -q "integrity: $packed" meridian.lock || fail "the pinned hash is not the packed module's hash"

say "a fresh machine fetches the pinned digest"
rm -rf .meridian
"$meridian" module install --locked
"$meridian" module verify

say "a run reads the module"
"$meridian" compile web.yaml @acme/hardening:documents/ssh.yaml \
  --data @acme/hardening:data/defaults.yaml --data data.yaml --out "$work/out"
grep -q '10-no-root.conf' "$work/out/ssh-baseline/playbook.yml" || fail "the module's document was not compiled"

say "a tampered module stops the run"
echo '# edited' >> .meridian/modules/acme/hardening/1.3.0/documents/ssh.yaml
if "$meridian" validate @acme/hardening:documents/ssh.yaml 2> "$work/tamper.txt"; then
  fail "a changed module was accepted"
fi
grep -q 'does not match meridian.lock' "$work/tamper.txt" || fail "the refusal does not say why"
"$meridian" module install --locked
"$meridian" validate @acme/hardening:documents/ssh.yaml

say "an image that is not a module is refused by name"
project "$work/not-a-module" "$open" "=9.9.9"
(cd "$work" && oras push --plain-http "$open:9.9.9" notes.txt:text/plain)
cd "$work/not-a-module"
if "$meridian" module install 2> "$work/image.txt"; then
  fail "something that is not a module was installed"
fi
grep -q 'is not a Meridian module' "$work/image.txt" || { cat "$work/image.txt"; fail "the refusal does not say why"; }

say "a signed module, through a registry"
"$meridian" signing keygen --secret "$work/publisher.key" --public "$work/publisher.pub"
cp -r "$root/examples/modules/hardening" "$work/src-3.0.0"
sed -i "s/^  version: .*/  version: 3.0.0/" "$work/src-3.0.0/meridian.yaml"
(cd "$work/src-3.0.0" && "$meridian" module sign --key "$work/publisher.key" && "$meridian" module push "$open" --insecure)
project "$work/signed" "$open" "^3.0"
cd "$work/signed"
"$meridian" module install --module-signatures require --trusted-key "$work/publisher.pub"
"$meridian" module verify --module-signatures require --trusted-key "$work/publisher.pub" | tee "$work/signed.txt"
grep -q 'acme/hardening 3.0.0  signed by key ' "$work/signed.txt" || fail "the signer was not reported"
# The signature is not part of what is pinned: the hash is the unsigned module's.
unsigned=$(cd "$work/src-3.0.0" && "$meridian" module pack --out "$work/signed.tar.gz" | sed -n 's/^  integrity:  //p')
grep -q "integrity: $unsigned" meridian.lock || fail "signing changed the pinned hash"
# The versions published before were not signed, and are refused here.
project "$work/unsigned" "$open" "^1.2"
cd "$work/unsigned"
if "$meridian" module install --module-signatures require --trusted-key "$work/publisher.pub" 2> "$work/unsigned.txt"; then
  fail "an unsigned module was installed where signatures are required"
fi
grep -q 'is not signed' "$work/unsigned.txt" || { cat "$work/unsigned.txt"; fail "the refusal does not say why"; }

say "a registry that wants a login"
private=localhost:5001/acme/meridian-hardening
export DOCKER_CONFIG="$work/docker"
mkdir -p "$DOCKER_CONFIG"
project "$work/private" "$private" "^1.2"
cd "$work/private"
if "$meridian" module install 2> "$work/denied.txt"; then
  fail "a private registry was read without a login"
fi
grep -q 'no login for it was found' "$work/denied.txt" || { cat "$work/denied.txt"; fail "the refusal does not say why"; }

say "a login the project declares, with no Docker login"
cat >> meridian.yaml <<YAML
      credential: private
  credentials:
    - name: private
      type: registry
      server: localhost:5001
      user: reader
      password:
        from: env
        key: MERIDIAN_TEST_REGISTRY_LOGIN
YAML
if "$meridian" module install 2> "$work/unset.txt"; then
  fail "a credential whose secret is not set was used"
fi
grep -q 'the credential private: ' "$work/unset.txt" || { cat "$work/unset.txt"; fail "the refusal does not name the credential"; }
# Published with the same credential, declared by the module being pushed.
cp -r "$work/src-1.2.0" "$work/src-declared"
sed -i "s/^  version: .*/  version: 1.2.5/" "$work/src-declared/meridian.yaml"
sed -n '/^  credentials:/,$p' meridian.yaml >> "$work/src-declared/meridian.yaml"
export MERIDIAN_TEST_REGISTRY_LOGIN=not-the-password
if (cd "$work/src-declared" && "$meridian" module push "$private" --insecure --credential private) 2> "$work/refused.txt"; then
  fail "a wrong password was accepted"
fi
grep -q 'the credential private was refused' "$work/refused.txt" || { cat "$work/refused.txt"; fail "the refusal does not say why"; }
export MERIDIAN_TEST_REGISTRY_LOGIN=s3cret
(cd "$work/src-declared" && "$meridian" module push "$private" --insecure --credential private)
"$meridian" module install | tee "$work/declared.txt"
grep -q '^+ acme/hardening 1.2.5 ' "$work/declared.txt" || fail "the module was not installed with the declared credential"
if grep -rq 's3cret' meridian.lock .meridian "$work/declared.txt"; then
  fail "the password was written down"
fi
"$meridian" module verify
unset MERIDIAN_TEST_REGISTRY_LOGIN
# A login for another registry is not sent to this one.
sed -i 's/server: localhost:5001/server: registry.example.com/' meridian.yaml
if MERIDIAN_TEST_REGISTRY_LOGIN=s3cret "$meridian" module update 2> "$work/elsewhere.txt"; then
  fail "a login for another registry was sent"
fi
grep -q 'it was not sent' "$work/elsewhere.txt" || { cat "$work/elsewhere.txt"; fail "the refusal does not say why"; }

say "a login from the Docker configuration"
project "$work/docker-login" "$private" "=1.2.0"
cd "$work/docker-login"
# The login is written the way `docker login` writes it. Without it the
# push is refused too.
if (cd "$work/src-1.2.0" && "$meridian" module push "$private" --insecure) 2> "$work/push-denied.txt"; then
  fail "a private registry was written to without a login"
fi
grep -q 'did not allow writing to' "$work/push-denied.txt" || { cat "$work/push-denied.txt"; fail "the refusal does not say why"; }
printf '{"auths":{"localhost:5001":{"auth":"%s"}}}\n' "$(printf 'reader:s3cret' | base64)" > "$DOCKER_CONFIG/config.json"
(cd "$work/src-1.2.0" && "$meridian" module push "$private" --insecure)
"$meridian" module install | tee "$work/private.txt"
grep -q '^+ acme/hardening 1.2.0 ' "$work/private.txt" || fail "the private module was not installed"
"$meridian" module verify

say "all passed"
