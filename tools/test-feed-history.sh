#!/usr/bin/env bash
# Regression tests for signed webOS preview feed history validation.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
VALIDATOR="$ROOT/tools/validate-feed-history.sh"
RENEWER="$ROOT/tools/renew-feed-metadata.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
for tool in jq openssl sha256sum; do command -v "$tool" >/dev/null; done
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$WORK/private.pem" >/dev/null 2>&1
openssl pkey -in "$WORK/private.pem" -pubout -out "$WORK/public.pem" >/dev/null
export RUNNER_TEMP="$WORK"
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }
make_fixture() {
  local dir="$WORK/feed"; rm -rf "$dir"; mkdir -p "$dir"
  printf 'test APKINDEX bytes\n' > "$dir/APKINDEX.tar.gz"
  printf 'test webOS APK bytes\n' > "$dir/couch-integration-webos-0.1.0_pre5-r0.apk"
  local is ih as ah
  is="$(stat -c '%s' "$dir/APKINDEX.tar.gz")"; ih="$(sha256sum "$dir/APKINDEX.tar.gz" | awk '{print $1}')"
  as="$(stat -c '%s' "$dir/couch-integration-webos-0.1.0_pre5-r0.apk")"; ah="$(sha256sum "$dir/couch-integration-webos-0.1.0_pre5-r0.apk" | awk '{print $1}')"
  jq -n --argjson is "$is" --arg ih "$ih" --argjson as "$as" --arg ah "$ah" '
    {schema:1,channel:"preview",sequence:2,index:{path:"APKINDEX.tar.gz",size:$is,sha256:$ih},
     packages:[{id:"webos",version:"0.1.0_pre5",apk:"couch-integration-webos-0.1.0_pre5-r0.apk",
     size:$as,sha256:$ah,protocol_version:5,min_core_protocol_version:5}]}
  ' > "$dir/feed.json"
  openssl dgst -sha256 -sign "$WORK/private.pem" -out "$dir/feed.json.sig" "$dir/feed.json"
}
sign_feed() { openssl dgst -sha256 -sign "$WORK/private.pem" -out "$WORK/feed/feed.json.sig" "$WORK/feed/feed.json"; }
run_ok() { bash "$VALIDATOR" "$WORK/feed" "$WORK/public.pem" "$1" "${@:2}" >/dev/null || fail "expected success for $1"; }
run_bad() { if bash "$VALIDATOR" "$WORK/feed" "$WORK/public.pem" "$1" "${@:2}" >/dev/null 2>&1; then fail "expected rejection for $1"; fi; }
make_fixture; run_ok 0.1.0_pre6; pass "valid signed prerelease feed and newer prerelease"
run_ok 0.1.0_pre5_git1; pass "fork build version sorts after its upstream prerelease"
run_bad 0.1.0_pre5.fork1; pass "non-APK .fork version syntax rejected"
run_ok 0.1.0_pre5 --allow-existing-version; pass "existing version allowed only for explicit dry run"
run_bad 0.1.0_pre5; pass "existing version rejected for publication"
run_bad 0.1.0_pre4; pass "older prerelease rejected"
make_fixture; printf 'tamper' >> "$WORK/feed/feed.json"; run_bad 0.1.0_pre6; pass "invalid feed signature rejected"
make_fixture; printf 'tamper' >> "$WORK/feed/APKINDEX.tar.gz"; run_bad 0.1.0_pre6; pass "APKINDEX size/hash mismatch rejected"
make_fixture; printf 'tamper' >> "$WORK/feed/couch-integration-webos-0.1.0_pre5-r0.apk"; run_bad 0.1.0_pre6; pass "historical APK hash mismatch rejected"
make_fixture; rm "$WORK/feed/couch-integration-webos-0.1.0_pre5-r0.apk"; run_bad 0.1.0_pre6; pass "missing historical APK rejected"
make_fixture; jq '.packages += .packages' "$WORK/feed/feed.json" > "$WORK/feed/modified.json"; mv "$WORK/feed/modified.json" "$WORK/feed/feed.json"; sign_feed; run_bad 0.1.0_pre6; pass "duplicate package/version rejected"
make_fixture; jq '.packages[0].apk = "couch-integration-webos-0.1.0_pre4-r0.apk"' "$WORK/feed/feed.json" > "$WORK/feed/modified.json"; mv "$WORK/feed/modified.json" "$WORK/feed/feed.json"; sign_feed; run_bad 0.1.0_pre6; pass "filename/version mismatch rejected"
make_fixture; jq '.sequence = 1.5' "$WORK/feed/feed.json" > "$WORK/feed/modified.json"; mv "$WORK/feed/modified.json" "$WORK/feed/feed.json"; sign_feed; run_bad 0.1.0_pre6; pass "non-integer sequence rejected"
make_fixture; jq '.packages[0].protocol_version = 3' "$WORK/feed/feed.json" > "$WORK/feed/modified.json"; mv "$WORK/feed/modified.json" "$WORK/feed/feed.json"; sign_feed; run_bad 0.1.0_pre6; pass "wrong Couch protocol version rejected"
# Metadata renewal must authenticate the current feed and preserve every package.
make_fixture
INDEX_BEFORE="$(sha256sum "$WORK/feed/APKINDEX.tar.gz" | awk '{print $1}')"
APK_BEFORE="$(sha256sum "$WORK/feed/couch-integration-webos-0.1.0_pre5-r0.apk" | awk '{print $1}')"
bash "$RENEWER" "$WORK/feed" "$WORK/private.pem" "$WORK/public.pem" >/dev/null
[[ "$(jq -r '.sequence' "$WORK/feed/feed.json")" == "3" ]] || fail "renewal did not increment sequence"
jq -e '.expires > .issued' "$WORK/feed/feed.json" >/dev/null || fail "renewal expiry is not after issue time"
openssl dgst -sha256 -verify "$WORK/public.pem" -signature "$WORK/feed/feed.json.sig" "$WORK/feed/feed.json" >/dev/null || fail "renewed feed signature does not verify"
[[ "$(sha256sum "$WORK/feed/APKINDEX.tar.gz" | awk '{print $1}')" == "$INDEX_BEFORE" ]] || fail "renewal changed APKINDEX"
[[ "$(sha256sum "$WORK/feed/couch-integration-webos-0.1.0_pre5-r0.apk" | awk '{print $1}')" == "$APK_BEFORE" ]] || fail "renewal changed published APK"
pass "metadata renewal increments sequence, re-signs metadata, and preserves packages"

# Fork suffixes must remain valid in both metadata and APK filenames during renewal.
make_fixture
mv "$WORK/feed/couch-integration-webos-0.1.0_pre5-r0.apk" "$WORK/feed/couch-integration-webos-0.1.0_pre5_git1-r0.apk"
jq '.packages[0].version = "0.1.0_pre5_git1" |
    .packages[0].apk = "couch-integration-webos-0.1.0_pre5_git1-r0.apk"' \
  "$WORK/feed/feed.json" > "$WORK/feed/modified.json"
mv "$WORK/feed/modified.json" "$WORK/feed/feed.json"
sign_feed
bash "$RENEWER" "$WORK/feed" "$WORK/private.pem" "$WORK/public.pem" >/dev/null || fail "renewal rejected valid fork package version"
pass "metadata renewal accepts APK-compatible fork suffix in metadata and filename"

openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$WORK/wrong-private.pem" >/dev/null 2>&1
make_fixture
if bash "$RENEWER" "$WORK/feed" "$WORK/wrong-private.pem" "$WORK/public.pem" >/dev/null 2>&1; then fail "renewal accepted mismatched signing keys"; fi
pass "metadata renewal rejects mismatched signing keys"

make_fixture
printf 'tamper' >> "$WORK/feed/couch-integration-webos-0.1.0_pre5-r0.apk"
if bash "$RENEWER" "$WORK/feed" "$WORK/private.pem" "$WORK/public.pem" >/dev/null 2>&1; then fail "renewal accepted a modified historical APK"; fi
pass "metadata renewal rejects modified historical APK"

echo "All webOS feed-history and renewal tests passed."
