#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
export PYTHONDONTWRITEBYTECODE=1

usage() {
  cat <<'EOF'
Usage: scripts/release.sh (--package-only | --publish | --preflight) [options]

Options:
  --channel stable|rc|nightly  Release channel (default: stable)
  --version VERSION            Must match Cargo.toml (default: package version)
  --build-id ID                Immutable id (default: wave-v<version>-<12-char HEAD>)
  --released-at ISO8601        Release timestamp (default: current UTC time)
  --endpoint URL               PortalSurfer origin (default: https://portalsurfer.org)
  --source-ref REF             Require a non-detached checkout of REF

Package-only and publish produce production bundles. Preflight uses an ad-hoc
signature and never contacts Apple or the PortalSurfer release API. Production
Apple signing/notarization credentials are read only from the environment (the
GitHub workflow supplies the same secrets used by Wavecrate):
APPLE_DEVELOPER_ID_APPLICATION_CERT_BASE64,
APPLE_DEVELOPER_ID_APPLICATION_CERT_PASSWORD, APPLE_NOTARY_KEY_BASE64,
APPLE_NOTARY_KEY_ID, and APPLE_NOTARY_ISSUER_ID.
EOF
}

mode=""
channel="stable"
requested_version=""
build_id=""
released_at=""
endpoint="https://portalsurfer.org"
source_ref=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --package-only|--publish|--preflight)
      [[ -z "${mode}" ]] || { echo "choose only one of --package-only, --publish, or --preflight" >&2; exit 2; }
      mode="${1#--}"; shift ;;
    --channel) channel="${2:?missing channel}"; shift 2 ;;
    --version) requested_version="${2:?missing version}"; shift 2 ;;
    --build-id) build_id="${2:?missing build id}"; shift 2 ;;
    --released-at) released_at="${2:?missing released-at}"; shift 2 ;;
    --endpoint) endpoint="${2:?missing endpoint}"; shift 2 ;;
    --source-ref) source_ref="${2:?missing source ref}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
[[ -n "${mode}" ]] || { usage >&2; exit 2; }
[[ "${channel}" == stable || "${channel}" == rc || "${channel}" == nightly ]] || { echo "invalid channel: ${channel}" >&2; exit 2; }

[[ "$(uname -s)" == "Darwin" ]] || { echo "release packaging requires macOS" >&2; exit 1; }

if [[ -n "${source_ref}" ]]; then
  current_ref="$(git symbolic-ref --quiet --short HEAD || true)"
  [[ -n "${current_ref}" ]] || { echo "--source-ref requires a non-detached checkout" >&2; exit 1; }
  [[ "${current_ref}" == "${source_ref}" ]] || { echo "requested source ${source_ref} does not match checkout ${current_ref}" >&2; exit 1; }
fi

if [[ -n "$(git status --porcelain --untracked-files=all)" ]]; then
  echo "release source must be clean" >&2
  git status --short >&2
  exit 1
fi

version="$(cargo metadata --locked --no-deps --format-version=1 --manifest-path "${repo_root}/Cargo.toml" \
  | python3 -c 'import json, pathlib, sys
manifest_path = pathlib.Path(sys.argv[1]).resolve()
for package in json.load(sys.stdin)["packages"]:
    if pathlib.Path(package["manifest_path"]).resolve() == manifest_path:
        print(package["version"])
        break
else:
    raise SystemExit(f"package manifest not found: {manifest_path}")' "${repo_root}/Cargo.toml"
)"
if [[ -n "${requested_version}" && "${requested_version}" != "${version}" ]]; then
  echo "requested version ${requested_version} does not match Cargo.toml ${version}" >&2
  exit 1
fi
version="${requested_version:-${version}}"
git_sha="$(git rev-parse HEAD)"
[[ "${git_sha}" =~ ^[0-9a-f]{40}$ ]] || { echo "could not resolve an exact source SHA" >&2; exit 1; }
if [[ "${mode}" != preflight ]]; then
  git fetch origin main --quiet
  canonical_main="$(git rev-parse refs/remotes/origin/main 2>/dev/null || true)"
  [[ -n "${canonical_main}" && "${git_sha}" == "${canonical_main}" ]] || { echo "production release source must equal origin/main (${canonical_main:-unavailable})" >&2; exit 1; }
fi
build_id="${build_id:-wave-v${version}-${git_sha:0:12}}"
[[ "${build_id}" =~ ^[a-z0-9][a-z0-9._-]{1,127}$ ]] || { echo "invalid build id" >&2; exit 2; }
released_at="${released_at:-$(date -u '+%Y-%m-%dT%H:%M:%SZ')}"
[[ -s CHANGELOG.md ]] || { echo "CHANGELOG.md must not be empty" >&2; exit 1; }
if [[ "${mode}" == publish && -z "${PORTALSURFER_RELEASE_TOKEN:-}" ]]; then
  echo "--publish requires PORTALSURFER_RELEASE_TOKEN (environment only)" >&2
  exit 1
fi
if [[ "${mode}" == publish && "${endpoint}" != "https://portalsurfer.org" ]]; then
  echo "production publishing requires exact origin https://portalsurfer.org" >&2
  exit 1
fi
if [[ ! -d "${VST3_SDK_DIR:-}" ]]; then
  echo "VST3_SDK_DIR must point to the VST3 SDK checkout" >&2
  exit 1
fi
echo "[release] running VST3 gate"
bash scripts/ci.sh --vst3
if [[ "${mode}" != preflight ]]; then
  for required in APPLE_DEVELOPER_ID_APPLICATION_CERT_BASE64 APPLE_DEVELOPER_ID_APPLICATION_CERT_PASSWORD APPLE_NOTARY_KEY_BASE64 APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER_ID; do
    [[ -n "${!required:-}" ]] || { echo "missing required Apple production credential: ${required}" >&2; exit 1; }
  done
fi

release_dir="${repo_root}/dist/releases/${build_id}"
rm -rf "${release_dir}"
mkdir -p "${release_dir}"
mkdir -p "${repo_root}/target"
tmp_root="$(mktemp -d "${repo_root}/target/release-build.XXXXXX")"
evidence_dir="${tmp_root}/notary-evidence"
mkdir -p "${evidence_dir}"
cleanup() {
  if [[ -f "${original_keychains_file:-}" ]]; then
    original_keychains=()
    while IFS= read -r keychain; do
      [[ -n "${keychain}" ]] && original_keychains+=("${keychain}")
    done < "${original_keychains_file}"
    if [[ "${#original_keychains[@]}" -gt 0 ]]; then
      security list-keychains -d user -s "${original_keychains[@]}" >/dev/null 2>&1 || true
    fi
  fi
  if [[ -n "${release_keychain:-}" ]]; then security delete-keychain "${release_keychain}" >/dev/null 2>&1 || true; fi
  rm -rf "${tmp_root}"
}
trap cleanup EXIT

signing_team_id=""
clap_notary_id=""
vst3_notary_id=""
if [[ "${mode}" != preflight ]]; then
  decode_base64() {
    if printf '%s' "$1" | base64 --decode > "$2" 2>/dev/null; then return 0; fi
    printf '%s' "$1" | base64 -D > "$2"
  }
  cert_path="${tmp_root}/developer-id-application.p12"
  notary_key_path="${tmp_root}/AuthKey_${APPLE_NOTARY_KEY_ID}.p8"
  decode_base64 "${APPLE_DEVELOPER_ID_APPLICATION_CERT_BASE64}" "${cert_path}"
  decode_base64 "${APPLE_NOTARY_KEY_BASE64}" "${notary_key_path}"
  chmod 600 "${cert_path}" "${notary_key_path}"
  release_keychain="${tmp_root}/wave-release.keychain-db"
  release_keychain_password="$(uuidgen | tr -d '-')"
  original_keychains_file="${tmp_root}/original-keychains.txt"
  security list-keychains -d user | sed 's/[[:space:]]*"//g; s/"$//' > "${original_keychains_file}"
  original_keychains=()
  while IFS= read -r keychain; do
    [[ -n "${keychain}" ]] && original_keychains+=("${keychain}")
  done < "${original_keychains_file}"
  security create-keychain -p "${release_keychain_password}" "${release_keychain}" >/dev/null
  security set-keychain-settings -lut 21600 "${release_keychain}"
  security unlock-keychain -p "${release_keychain_password}" "${release_keychain}"
  security list-keychains -d user -s "${release_keychain}" "${original_keychains[@]}" >/dev/null
  security import "${cert_path}" -P "${APPLE_DEVELOPER_ID_APPLICATION_CERT_PASSWORD}" -A -t cert -f pkcs12 -k "${release_keychain}" >/dev/null
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "${release_keychain_password}" "${release_keychain}" >/dev/null
  codesign_identity="${APPLE_CODESIGN_IDENTITY:-}"
  if [[ -z "${codesign_identity}" ]]; then
    codesign_identity="$(security find-identity -v -p codesigning "${release_keychain}" | sed -n 's/.*"\(Developer ID Application:.*\)".*/\1/p' | head -n 1)"
  fi
  [[ "${codesign_identity}" == Developer\ ID\ Application:* ]] || { echo "no Developer ID Application identity found" >&2; exit 1; }
fi
if [[ "${mode}" == preflight ]]; then codesign_identity="-"; fi

echo "[release] rendering WAVE screenshots"
rm -rf target/ui-screenshots
bash scripts/ci.sh --screenshots
png="target/ui-screenshots/wave/initial-ui-default.png"
[[ -f "${png}" ]] || { echo "default screenshot was not produced" >&2; exit 1; }
cp "${png}" "${release_dir}/wave-default-960x600.png"

build_bundle() {
  local format="$1" target_dir="$2" bundle_dir="$3" binary="$4"
  local contents="${bundle_dir}/Contents"
  mkdir -p "${contents}/MacOS"
  cp "${binary}" "${contents}/MacOS/wave"
  chmod 755 "${contents}/MacOS/wave"
  cat > "${contents}/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>wave</string>
<key>CFBundleIdentifier</key><string>com.portalsurfer.wave.${format}</string>
<key>CFBundleName</key><string>WAVE</string>
<key>CFBundlePackageType</key><string>BNDL</string>
<key>CFBundleShortVersionString</key><string>${version}</string>
<key>CFBundleVersion</key><string>${version}</string>
</dict></plist>
EOF
  printf 'BNDL????' > "${contents}/PkgInfo"
  /usr/bin/plutil -lint "${contents}/Info.plist" >/dev/null
  if [[ "${mode}" == preflight ]]; then
    printf 'preflight CodeResources\n' > "${contents}/CodeResources"
    codesign --force --deep --sign - "${bundle_dir}" >/dev/null
    codesign --verify --deep --strict "${bundle_dir}"
  else
    codesign --force --deep --timestamp --options runtime --keychain "${release_keychain}" --sign "${codesign_identity}" "${bundle_dir}" >/dev/null
    codesign --verify --deep --strict "${bundle_dir}"
    local notarize_zip="${tmp_root}/notary-${format}.zip"
    /usr/bin/ditto -c -k --sequesterRsrc --keepParent "${bundle_dir}" "${notarize_zip}"
    local notary_json="${tmp_root}/notary-${format}.json"
    xcrun notarytool submit "${notarize_zip}" --key "${notary_key_path}" --key-id "${APPLE_NOTARY_KEY_ID}" --issuer "${APPLE_NOTARY_ISSUER_ID}" --wait --output-format json >"${notary_json}"
    local notary_status notary_id
    notary_status="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("status", ""))' "${notary_json}")"
    notary_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id", ""))' "${notary_json}")"
    [[ "${notary_status}" == Accepted && -n "${notary_id}" ]] || { echo "notarization was not accepted for ${format}" >&2; cat "${notary_json}" >&2; exit 1; }
    local notary_log="${evidence_dir}/notary-${format}-${notary_id}.json"
    xcrun notarytool log "${notary_id}" --key "${notary_key_path}" --key-id "${APPLE_NOTARY_KEY_ID}" --issuer "${APPLE_NOTARY_ISSUER_ID}" --output-format json >"${notary_log}"
    python3 - "${notary_log}" "${format}" <<'PY'
import json
import sys

path, format_name = sys.argv[1:]
payload = json.load(open(path, encoding="utf-8"))
issues = payload.get("issues") or []
errors = []
for issue in issues:
    severity = str(issue.get("severity", "")).lower()
    message = str(issue.get("message", "")).strip()
    if severity == "error":
        errors.append(message or "unspecified notary issue")
    elif severity == "warning":
        print(f"[release] notarization warning ({format_name}): {message or 'unspecified warning'}", file=sys.stderr)
if errors:
    for message in errors:
        print(f"[release] notarization error ({format_name}): {message}", file=sys.stderr)
    raise SystemExit(1)
PY
    if [[ "${format}" == clap ]]; then clap_notary_id="${notary_id}"; else vst3_notary_id="${notary_id}"; fi
    xcrun stapler staple "${bundle_dir}" >/dev/null
    xcrun stapler validate "${bundle_dir}" >/dev/null
    codesign -vvvv -R=notarized --check-notarization "${bundle_dir}" >/dev/null
    local team_id
    team_id="$(codesign -dv --verbose=4 "${bundle_dir}" 2>&1 | sed -n 's/^TeamIdentifier=//p' | head -n 1)"
    [[ -n "${team_id}" ]] || { echo "could not capture Developer ID team identifier" >&2; exit 1; }
    if [[ -z "${signing_team_id}" ]]; then signing_team_id="${team_id}"; elif [[ "${signing_team_id}" != "${team_id}" ]]; then echo "bundle signing team identifiers differ" >&2; exit 1; fi
  fi
  file "${contents}/MacOS/wave" | grep -q 'arm64' || { echo "${format} binary is not arm64" >&2; exit 1; }
  if command -v lipo >/dev/null 2>&1; then
    [[ "$(lipo -archs "${contents}/MacOS/wave")" == "arm64" ]] || { echo "${format} binary must contain only arm64" >&2; exit 1; }
  fi
  if [[ "${format}" == clap ]]; then
    /usr/bin/nm -gU "${contents}/MacOS/wave" | grep -q '_clap_entry' || { echo "CLAP entrypoint missing" >&2; exit 1; }
  else
    for symbol in _GetPluginFactory _bundleEntry _bundleExit; do
      /usr/bin/nm -gU "${contents}/MacOS/wave" | grep -q "${symbol}" || { echo "VST3 symbol ${symbol} missing" >&2; exit 1; }
    done
  fi
  /usr/bin/ditto -c -k --sequesterRsrc --keepParent "${bundle_dir}" "${target_dir}"
}

audit_zip() {
  local format="$1" archive="$2" expected_team="$3" audit_dir bundle contents codesign_details team_id
  if [[ "${mode}" == preflight ]]; then
    python3 - "${archive}" "${format}" <<'PY'
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path("scripts").resolve()))
from release_helper import _audit_zip

_audit_zip(pathlib.Path(sys.argv[1]), sys.argv[2], "", cwd=pathlib.Path.cwd(), require_production=False)
PY
    return
  fi
  audit_dir="$(mktemp -d "${tmp_root}/audit.XXXXXX")"
  /usr/bin/ditto -x -k "${archive}" "${audit_dir}"
  bundle="${audit_dir}/wave.${format}"
  contents="${bundle}/Contents"
  [[ -f "${contents}/Info.plist" && -f "${contents}/PkgInfo" && -x "${contents}/MacOS/wave" ]] || { echo "${format} ZIP bundle layout is invalid" >&2; exit 1; }
  /usr/bin/plutil -lint "${contents}/Info.plist" >/dev/null
  [[ "$(/usr/bin/plutil -extract CFBundleIdentifier raw -o - "${contents}/Info.plist")" == "com.portalsurfer.wave.${format}" ]] || { echo "${format} ZIP bundle identifier is invalid" >&2; exit 1; }
  [[ "$(/usr/bin/plutil -extract CFBundlePackageType raw -o - "${contents}/Info.plist")" == "BNDL" ]] || { echo "${format} ZIP package type is invalid" >&2; exit 1; }
  codesign --verify --deep --strict "${bundle}"
  xcrun stapler validate "${bundle}" >/dev/null
  codesign -vvvv -R=notarized --check-notarization "${bundle}" >/dev/null
  codesign_details="$(codesign -dv --verbose=4 "${bundle}" 2>&1)"
  printf '%s\n' "${codesign_details}" | grep -q '^Authority=Developer ID Application:' || { echo "${format} ZIP is not signed by a Developer ID Application authority" >&2; exit 1; }
  team_id="$(printf '%s\n' "${codesign_details}" | sed -n 's/^TeamIdentifier=//p' | head -n 1)"
  [[ -n "${team_id}" && "${team_id}" == "${expected_team}" ]] || { echo "${format} ZIP Developer ID signing team does not match manifest" >&2; exit 1; }
  file "${contents}/MacOS/wave" | grep -q arm64 || { echo "${format} ZIP binary is not arm64" >&2; exit 1; }
  if command -v lipo >/dev/null 2>&1; then [[ "$(lipo -archs "${contents}/MacOS/wave")" == arm64 ]] || { echo "${format} ZIP binary is not arm64-only" >&2; exit 1; }; fi
  if [[ "${format}" == clap ]]; then /usr/bin/nm -gU "${contents}/MacOS/wave" | grep -q _clap_entry || { echo "CLAP ZIP entrypoint missing" >&2; exit 1; }; else
    for symbol in _GetPluginFactory _bundleEntry _bundleExit; do /usr/bin/nm -gU "${contents}/MacOS/wave" | grep -q "${symbol}" || { echo "VST3 ZIP symbol ${symbol} missing" >&2; exit 1; }; done
  fi
}

clap_target="${tmp_root}/clap-target"
vst3_target="${tmp_root}/vst3-target"
echo "[release] building CLAP"
TOYBOX_ACTIVE_ARTIFACT=clap CARGO_TARGET_DIR="${clap_target}" cargo build --locked --release
clap_binary="${clap_target}/release/libwave.dylib"
[[ -f "${clap_binary}" ]] || { echo "CLAP build did not produce ${clap_binary}" >&2; exit 1; }
clap_bundle="${tmp_root}/wave.clap"
build_bundle clap "${release_dir}/wave-v${version}-macos.clap.zip" "${clap_bundle}" "${clap_binary}"
audit_zip clap "${release_dir}/wave-v${version}-macos.clap.zip" "${signing_team_id}"

echo "[release] building VST3"
TOYBOX_ACTIVE_ARTIFACT=vst3 VST3_SDK_DIR="${VST3_SDK_DIR}" CARGO_TARGET_DIR="${vst3_target}" cargo rustc --locked --release --features vst3 -- -C link-arg=-Wl,-bundle
vst3_binary="${vst3_target}/release/libwave.dylib"
[[ -f "${vst3_binary}" ]] || { echo "VST3 build did not produce ${vst3_binary}" >&2; exit 1; }
vst3_bundle="${tmp_root}/wave.vst3"
build_bundle vst3 "${release_dir}/wave-v${version}-macos.vst3.zip" "${vst3_bundle}" "${vst3_binary}"
audit_zip vst3 "${release_dir}/wave-v${version}-macos.vst3.zip" "${signing_team_id}"

cp CHANGELOG.md "${release_dir}/CHANGELOG.md"
if [[ "${mode}" == preflight ]]; then
  python3 - "${release_dir}" "${version}" "${build_id}" "${channel}" "${released_at}" "${git_sha}" <<'PY'
import pathlib
import sys
sys.path.insert(0, str(pathlib.Path("scripts").resolve()))
from release_helper import build_manifest, canonical_json, validate_preflight_manifest
root = pathlib.Path(sys.argv[1])
manifest = build_manifest(version=sys.argv[2], build_id=sys.argv[3], channel=sys.argv[4], released_at=sys.argv[5], git_sha=sys.argv[6], clap=root / f"wave-v{sys.argv[2]}-macos.clap.zip", vst3=root / f"wave-v{sys.argv[2]}-macos.vst3.zip", screenshot=root / "wave-default-960x600.png", changelog=root / "CHANGELOG.md", distribution="preflight", signing_identity_class="ad hoc", notarized=False, stapled=False)
(root / "release-manifest.json").write_bytes(canonical_json(manifest))
validate_preflight_manifest(manifest, root)
PY
else
  python3 - "${release_dir}" "${version}" "${build_id}" "${channel}" "${released_at}" "${git_sha}" "${signing_team_id}" "${clap_notary_id}" "${vst3_notary_id}" <<'PY'
import json, pathlib, sys
sys.path.insert(0, str(pathlib.Path("scripts").resolve()))
from release_helper import build_manifest, canonical_json
root = pathlib.Path(sys.argv[1])
manifest = build_manifest(version=sys.argv[2], build_id=sys.argv[3], channel=sys.argv[4], released_at=sys.argv[5], git_sha=sys.argv[6], clap=root / f"wave-v{sys.argv[2]}-macos.clap.zip", vst3=root / f"wave-v{sys.argv[2]}-macos.vst3.zip", screenshot=root / "wave-default-960x600.png", changelog=root / "CHANGELOG.md", distribution="production", signing_identity_class="Developer ID Application", notarized=True, stapled=True, signing_team_id=sys.argv[7], notary_submissions={"clap": sys.argv[8], "vst3": sys.argv[9]})
(root / "release-manifest.json").write_bytes(canonical_json(manifest))
PY
fi

if [[ "${mode}" == publish ]]; then
  python3 - "${release_dir}" "${endpoint}" <<'PY'
import os
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path("scripts").resolve()))
from release_helper import publish_release

root = pathlib.Path(sys.argv[1])
publish_release(
    endpoint=sys.argv[2],
    token=os.environ.get("PORTALSURFER_RELEASE_TOKEN", ""),
    manifest_path=root / "release-manifest.json",
    root=root,
    repo_root=pathlib.Path.cwd(),
)
PY
fi

echo "[release] bundle ready: ${release_dir}"
find "${release_dir}" -maxdepth 1 -type f -print | sort
