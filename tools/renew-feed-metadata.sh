#!/usr/bin/env bash
# Renew signed webOS feed metadata without rebuilding or changing packages.
set -Eeuo pipefail
usage() { printf 'Usage: %s FEED_ARCH_DIR PRIVATE_KEY PUBLIC_KEY\n' "$0" >&2; exit 64; }
[[ "$#" == 3 ]] || usage
FEED_DIR="$(cd "$1" && pwd -P)"; PRIVATE_KEY="$2"; PUBLIC_KEY="$3"
FEED_JSON="$FEED_DIR/feed.json"; FEED_SIGNATURE="$FEED_DIR/feed.json.sig"; APK_INDEX="$FEED_DIR/APKINDEX.tar.gz"
for path in "$PRIVATE_KEY" "$PUBLIC_KEY" "$FEED_JSON" "$FEED_SIGNATURE" "$APK_INDEX"; do
  [[ -s "$path" ]] || { printf 'ERROR: required feed/key file is missing or empty: %s\n' "$path" >&2; exit 1; }
done
for tool in jq openssl sha256sum; do command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: $tool is required." >&2; exit 1; }; done
PRIVATE_DER_SHA256="$(openssl pkey -in "$PRIVATE_KEY" -pubout -outform DER 2>/dev/null | sha256sum | awk '{print $1}')"
PUBLIC_DER_SHA256="$(openssl pkey -pubin -in "$PUBLIC_KEY" -outform DER 2>/dev/null | sha256sum | awk '{print $1}')"
[[ "$PRIVATE_DER_SHA256" =~ ^[[:xdigit:]]{64}$ && "$PRIVATE_DER_SHA256" == "$PUBLIC_DER_SHA256" ]] || { echo 'ERROR: feed signing private/public keys do not match.' >&2; exit 1; }
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
    (.apk | type == "string" and test("^couch-integration-webos-[0-9]+\\.[0-9]+\\.[0-9]+(_pre[0-9]+)?(_git[0-9]+)?-r0\\.apk$")) and
    (.size | type == "number" and . > 0 and . == floor) and
    (.sha256 | type == "string" and test("^[0-9a-f]{64}$"))
  ) and
  ([.packages[].apk] | unique | length) == (.packages | length) and
  ([.packages[].version] | unique | length) == (.packages | length)
' "$FEED_JSON" >/dev/null || { echo 'ERROR: existing feed metadata failed validation.' >&2; exit 1; }
openssl dgst -sha256 -verify "$PUBLIC_KEY" -signature "$FEED_SIGNATURE" "$FEED_JSON" >/dev/null || { echo 'ERROR: existing feed metadata signature did not verify.' >&2; exit 1; }
OLD_INDEX_SIZE="$(jq -er '.index.size | numbers' "$FEED_JSON")"; OLD_INDEX_SHA256="$(jq -er '.index.sha256 | strings' "$FEED_JSON")"
INDEX_SIZE="$(stat -c '%s' "$APK_INDEX")"; INDEX_SHA256="$(sha256sum "$APK_INDEX" | awk '{print $1}')"
[[ "$OLD_INDEX_SIZE" == "$INDEX_SIZE" && "$OLD_INDEX_SHA256" == "$INDEX_SHA256" ]] || { echo 'ERROR: existing feed metadata does not describe current APKINDEX.' >&2; exit 1; }
while IFS=$'\t' read -r METADATA_VERSION APK_NAME EXPECTED_SIZE EXPECTED_SHA256; do
  [[ "$APK_NAME" =~ ^couch-integration-webos-[0-9]+\.[0-9]+\.[0-9]+(_pre[0-9]+)?(_git[0-9]+)?-r0\.apk$ ]] || { echo 'ERROR: feed contains invalid APK filename.' >&2; exit 1; }
  FILENAME_VERSION="${APK_NAME#couch-integration-webos-}"; FILENAME_VERSION="${FILENAME_VERSION%-r0.apk}"
  [[ "$METADATA_VERSION" == "$FILENAME_VERSION" ]] || { echo "ERROR: metadata version does not match APK filename: $APK_NAME" >&2; exit 1; }
  APK_PATH="$FEED_DIR/$APK_NAME"; [[ -s "$APK_PATH" ]] || { printf 'ERROR: published APK is missing: %s\n' "$APK_NAME" >&2; exit 1; }
  ACTUAL_SIZE="$(stat -c '%s' "$APK_PATH")"; ACTUAL_SHA256="$(sha256sum "$APK_PATH" | awk '{print $1}')"
  [[ "$EXPECTED_SIZE" == "$ACTUAL_SIZE" && "$EXPECTED_SHA256" == "$ACTUAL_SHA256" ]] || { printf 'ERROR: published APK does not match metadata: %s\n' "$APK_NAME" >&2; exit 1; }
done < <(jq -er '.packages[] | [.version, .apk, (.size | tostring), .sha256] | @tsv' "$FEED_JSON")
SEQUENCE="$(jq -er '.sequence | numbers' "$FEED_JSON")"
[[ "$SEQUENCE" =~ ^[0-9]+$ && "$SEQUENCE" -lt 2147483647 ]] || { echo 'ERROR: feed sequence is invalid or exhausted.' >&2; exit 1; }
NEXT_SEQUENCE="$((SEQUENCE + 1))"; ISSUED="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"; EXPIRES="$(date -u -d '+30 days' +'%Y-%m-%dT%H:%M:%SZ')"
TEMP_FEED="$(mktemp "$FEED_DIR/.feed.json.XXXXXX")"; TEMP_SIGNATURE="$(mktemp "$FEED_DIR/.feed.json.sig.XXXXXX")"
cleanup() { rm -f "$TEMP_FEED" "$TEMP_SIGNATURE"; }; trap cleanup EXIT HUP INT TERM
jq --argjson sequence "$NEXT_SEQUENCE" --arg issued "$ISSUED" --arg expires "$EXPIRES" --argjson index_size "$INDEX_SIZE" --arg index_sha256 "$INDEX_SHA256" '.sequence = $sequence | .issued = $issued | .expires = $expires | .index.size = $index_size | .index.sha256 = $index_sha256' "$FEED_JSON" > "$TEMP_FEED"
openssl dgst -sha256 -sign "$PRIVATE_KEY" -out "$TEMP_SIGNATURE" "$TEMP_FEED"
openssl dgst -sha256 -verify "$PUBLIC_KEY" -signature "$TEMP_SIGNATURE" "$TEMP_FEED" >/dev/null
mv "$TEMP_FEED" "$FEED_JSON"; mv "$TEMP_SIGNATURE" "$FEED_SIGNATURE"; trap - EXIT HUP INT TERM
printf 'Renewed webOS preview feed metadata: sequence %s, expires %s\n' "$NEXT_SEQUENCE" "$EXPIRES"
