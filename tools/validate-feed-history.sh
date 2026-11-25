#!/usr/bin/env bash
# Authenticate the existing webOS preview feed before carrying history forward.
set -Eeuo pipefail
usage() { echo "Usage: $0 FEED_ARCH_DIR REPOSITORY_PUBLIC_KEY NEW_VERSION [--allow-existing-version]" >&2; exit 64; }
[[ "$#" == 3 || "$#" == 4 ]] || usage
FEED_DIR="$1"; PUBLIC_KEY="$2"; NEW_VERSION="$3"
ALLOW_EXISTING_VERSION=false
if [[ "$#" == 4 ]]; then
  [[ "$4" == "--allow-existing-version" ]] || usage
  ALLOW_EXISTING_VERSION=true
fi
VERSION_RE='^[0-9]+\.[0-9]+\.[0-9]+(_pre[0-9]+)?(_git[0-9]+)?$'
[[ "$NEW_VERSION" =~ $VERSION_RE ]] || { echo "ERROR: invalid webOS release version: $NEW_VERSION" >&2; exit 1; }
FEED="$FEED_DIR/feed.json"; SIGNATURE="$FEED_DIR/feed.json.sig"; INDEX="$FEED_DIR/APKINDEX.tar.gz"
for file in "$FEED" "$SIGNATURE" "$INDEX" "$PUBLIC_KEY"; do
  [[ -s "$file" ]] || { echo "ERROR: missing or empty file: $file" >&2; exit 1; }
done
command -v jq >/dev/null; command -v openssl >/dev/null; command -v sha256sum >/dev/null
openssl dgst -sha256 -verify "$PUBLIC_KEY" -signature "$SIGNATURE" "$FEED" >/dev/null || {
  echo "ERROR: existing feed signature is invalid." >&2; exit 1;
}
jq -e '
  .schema == 1 and .channel == "preview" and
  (.sequence | type == "number" and . >= 1 and . == floor and . < 2147483647) and
  .index.path == "APKINDEX.tar.gz" and
  (.index.size | type == "number" and . > 0 and . == floor) and
  (.index.sha256 | type == "string" and test("^[0-9a-f]{64}$")) and
  (.packages | type == "array" and length > 0) and
  all(.packages[];
    .id == "webos" and .protocol_version == 5 and .min_core_protocol_version == 5 and
    (.version | type == "string" and test("^[0-9]+\\.[0-9]+\\.[0-9]+(_pre[0-9]+)?(_git[0-9]+)?$")) and
    (.apk == ("couch-integration-webos-" + .version + "-r0.apk")) and
    (.size | type == "number" and . > 0 and . == floor) and
    (.sha256 | type == "string" and test("^[0-9a-f]{64}$"))
  ) and
  ([.packages[].apk] | unique | length) == (.packages | length) and
  ([.packages[].version] | unique | length) == (.packages | length)
' "$FEED" >/dev/null || { echo "ERROR: existing feed metadata failed validation." >&2; exit 1; }
[[ "$(stat -c '%s' "$INDEX")" == "$(jq -r '.index.size' "$FEED")" ]] || { echo "ERROR: APKINDEX size mismatch." >&2; exit 1; }
[[ "$(sha256sum "$INDEX" | awk '{print $1}')" == "$(jq -r '.index.sha256' "$FEED")" ]] || { echo "ERROR: APKINDEX hash mismatch." >&2; exit 1; }
PACKAGE_LIST="${RUNNER_TEMP:-/tmp}/webos-verified-packages.tsv"
PACKAGE_LIST_TMP="$(mktemp "${PACKAGE_LIST}.XXXXXX")"
trap 'rm -f "$PACKAGE_LIST_TMP"' EXIT
while read -r APK_NAME EXPECTED_SIZE EXPECTED_SHA; do
  APK="$FEED_DIR/$APK_NAME"
  [[ -s "$APK" ]] || { echo "ERROR: historical APK missing: $APK_NAME" >&2; exit 1; }
  [[ "$(stat -c '%s' "$APK")" == "$EXPECTED_SIZE" ]] || { echo "ERROR: historical APK size mismatch: $APK_NAME" >&2; exit 1; }
  [[ "$(sha256sum "$APK" | awk '{print $1}')" == "$EXPECTED_SHA" ]] || { echo "ERROR: historical APK hash mismatch: $APK_NAME" >&2; exit 1; }
done < <(jq -r '.packages[] | [.apk, (.size|tostring), .sha256] | @tsv' "$FEED")
jq -r '.packages[] | [.version, .apk, (.size|tostring), .sha256] | @tsv' "$FEED" > "$PACKAGE_LIST_TMP"
mapfile -t VERSIONS < <(jq -r '.packages[].version' "$FEED" | sort -V)
UNIQUE_COUNT="$(printf '%s\n' "${VERSIONS[@]}" | sort -u | wc -l)"
[[ "$UNIQUE_COUNT" -eq "${#VERSIONS[@]}" ]] || { echo "ERROR: duplicate versions exist in feed history." >&2; exit 1; }
HIGHEST="${VERSIONS[-1]}"
LATEST="$(printf '%s\n' "$HIGHEST" "$NEW_VERSION" | sort -V | tail -n 1)"
if [[ "$NEW_VERSION" == "$HIGHEST" && "$ALLOW_EXISTING_VERSION" == true ]]; then
  echo "Dry-run validation: rebuilding existing version $NEW_VERSION; publication remains disabled."
elif [[ "$LATEST" != "$NEW_VERSION" || "$NEW_VERSION" == "$HIGHEST" ]]; then
  echo "ERROR: release $NEW_VERSION must be newer than existing version $HIGHEST." >&2
  exit 1
fi
mv "$PACKAGE_LIST_TMP" "$PACKAGE_LIST"
trap - EXIT
echo "Existing webOS feed signature, metadata, APK history, and version order verified."