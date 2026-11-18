#!/usr/bin/env bash
# Publishes a signed schema bundle to real OCI registries and has a project
# take its schemas from it, the way docs/tooling/schema-bundles.md says to.
#
# It expects an open registry on localhost:5000 and one that wants the login
# reader / s3cret on localhost:5001, and `oras` on the PATH, which is used to
# look at what was published. The modules workflow starts all three; see
# .github/workflows/modules.yml. The Ansible plugin must sit beside meridian.
#
#   tools/real_schema_registry_test.sh /path/to/meridian
set -euo pipefail

meridian=${1:?usage: real_schema_registry_test.sh /path/to/meridian}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export MERIDIAN_ANNOTATIONS=off
unset MERIDIAN_STATE_DIR MERIDIAN_SCHEMA_SIGNATURES MERIDIAN_SCHEMA_PREFER

say() { printf '\n== %s\n' "$*"; }

fail() {
  echo "FAILED: $*" >&2
  exit 1
}

document() {
  cat > "$1/site.yaml" <<'YAML'
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: motd
      type: ansible.builtin.lineinfile
      params:
        path: /etc/motd
        line: managed by meridian
YAML
}

# The cache a sync would have written, by hand: the cloud runner has no
# ansible-doc, and this test is about the registry, not the exporter.
cache() {
  mkdir -p "$1/.meridian/schemas"
  cat > "$1/.meridian/schemas/ansible.json" <<'JSON'
{
  "cacheVersion": 1,
  "target": "ansible",
  "tool": "ansible-doc",
  "toolVersion": "2.17.0",
  "pins": { "ansible.builtin": "2.17.0" },
  "syncedAt": "2026-11-18T00:00:00Z",
  "notes": [],
  "schemas": [
    {
      "type": "ansible.builtin.lineinfile",
      "description": "Manage lines in text files.",
      "params": [
        { "name": "path", "kind": "string", "required": true },
        { "name": "line", "kind": "string" },
        { "name": "state", "kind": "string", "values": ["absent", "present"] }
      ]
    },
    { "type": "ansible.builtin.copy", "description": "Copy files to remote locations.", "open": true }
  ]
}
JSON
}

say "pack and sign a bundle, and push it to an open registry"
open=localhost:5000/acme/meridian-schemas
publisher="$work/publisher"
mkdir -p "$publisher"
document "$publisher"
cache "$publisher"
cd "$publisher"
"$meridian" signing keygen --secret "$work/schemas.key" --public "$work/schemas.pub"
"$meridian" schema pack ansible --name builtin --version 2.17.0 --key "$work/schemas.key" \
  --out "$work/builtin.tar.gz" | tee "$work/pack.txt"
grep -q 'packed ansible/builtin:2.17.0 (2 resource types' "$work/pack.txt" || fail "the bundle was not packed"
"$meridian" schema push "$work/builtin.tar.gz" "oci://$open" --insecure | tee "$work/push.txt"
grep -q "reference: oci+$open/ansible/builtin:2.17.0" "$work/push.txt" || fail "the push did not say where"
digest=$(sed -n 's/^  digest:    //p' "$work/push.txt")
"$meridian" schema push "$work/builtin.tar.gz" "$open" --insecure | grep -q 'is already published' \
  || fail "a second push of the same bundle was not recognised"

say "what the registry holds is a schema bundle and an index"
oras manifest fetch --plain-http "$open/ansible/builtin:2.17.0" > "$work/manifest.json"
grep -q '"artifactType":"application/vnd.meridian.schema-bundle.v1"' "$work/manifest.json" \
  || { cat "$work/manifest.json"; fail "the bundle's artifact type"; }
oras manifest fetch --plain-http "$open/index:latest" | grep -q 'application/vnd.meridian.schema-index.v1' \
  || fail "the index's artifact type"
oras pull --plain-http "$open/index:latest" -o "$work/index"
grep -q "\"digest\": \"$digest\"" "$work/index/index.json" || { cat "$work/index/index.json"; fail "the index does not list the bundle"; }

say "a project takes its schemas from the registry, signatures required"
site="$work/site"
mkdir -p "$site"
document "$site"
cat > "$site/meridian.yaml" <<YAML
apiVersion: meridian/v1
kind: Module
metadata:
  name: site
spec:
  schemas:
    registry: $open
    insecure: true
    signatures: require
YAML
cd "$site"
if "$meridian" schema sync site.yaml 2> "$work/untrusted.txt"; then
  fail "a bundle signed by an untrusted key was used"
fi
grep -q 'not a trusted key' "$work/untrusted.txt" || { cat "$work/untrusted.txt"; fail "the refusal does not say why"; }
"$meridian" schema sync --trusted-key "$work/schemas.pub" site.yaml | tee "$work/sync.txt"
grep -q 'from the schema bundle ansible/builtin:2.17.0' "$work/sync.txt" || fail "the bundle was not used"
grep -q "reference: oci+$open/ansible/builtin:2.17.0" meridian.lock || fail "the lock does not name the bundle"
grep -q "digest: \"\\?$digest" meridian.lock || fail "the lock does not pin the digest"
"$meridian" schema sync --check site.yaml

say "a fresh machine fetches exactly what the lock pins"
rm -rf .meridian
"$meridian" schema sync --locked --trusted-key "$work/schemas.pub" site.yaml
"$meridian" schema sync --check site.yaml
"$meridian" lint site.yaml

say "a bundle fetched by reference"
fetched="$work/fetched"
mkdir -p "$fetched"
cd "$fetched"
"$meridian" schema fetch "oci://$open/ansible/builtin:2.17.0" --schema-signatures require \
  --trusted-key "$work/schemas.pub" --no-lock | tee "$work/fetch.txt"
grep -q 'cached 2 resource types' "$work/fetch.txt" || fail "fetch did not write the cache"

say "a registry that wants a login"
private=localhost:5001/acme/meridian-schemas
export DOCKER_CONFIG="$work/docker"
mkdir -p "$DOCKER_CONFIG"
login_manifest() {
  cat > "$1/meridian.yaml" <<YAML
apiVersion: meridian/v1
kind: Module
metadata:
  name: site
spec:
  credentials:
    - name: schemas
      type: registry
      server: localhost:5001
      user: reader
      password:
        from: env
        key: MERIDIAN_TEST_REGISTRY_LOGIN
$2
YAML
}
login_manifest "$publisher" ""
cd "$publisher"
if "$meridian" schema push "$work/builtin.tar.gz" "$private" --insecure 2> "$work/denied.txt"; then
  fail "a private registry was written to without a login"
fi
grep -q 'did not allow writing to' "$work/denied.txt" || { cat "$work/denied.txt"; fail "the refusal does not say why"; }
MERIDIAN_TEST_REGISTRY_LOGIN=s3cret "$meridian" schema push "$work/builtin.tar.gz" "$private" \
  --insecure --credential schemas
private_site="$work/private-site"
mkdir -p "$private_site"
document "$private_site"
login_manifest "$private_site" "  schemas:
    registry: $private
    credential: schemas
    insecure: true"
cd "$private_site"
MERIDIAN_TEST_REGISTRY_LOGIN=s3cret "$meridian" schema sync site.yaml | tee "$work/private.txt"
grep -q 'from the schema bundle ansible/builtin:2.17.0' "$work/private.txt" || fail "the private bundle was not used"
if grep -rq 's3cret' meridian.lock .meridian "$work/private.txt"; then
  fail "the password was written down"
fi

say "all passed"
