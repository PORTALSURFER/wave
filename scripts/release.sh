#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
export PYTHONDONTWRITEBYTECODE=1

slug="wave"
endpoint="https://portalsurfer.org"
mode=""
channel="stable"
requested_version=""
requested_publication_version=""
build_id=""
released_at=""
source_ref=""
requested_source_sha=""
windows_release_dir=""
publisher_script=""
vst3_sdk_revision="58f8da7936800732561402d7936584ca4505de07"

usage() {
  cat <<'EOF'
Usage: scripts/release.sh (--package-only | --publish | --preflight) [options]

Options:
  --channel stable|rc|nightly  Release channel (default: stable)
  --version VERSION            Must match Cargo.toml
  --publication-version VERSION
                               Publication identity; core must match Cargo.toml
  --build-id ID                Immutable id (default: wave-v<publication-version>-<12-char source SHA>)
  --released-at ISO8601        Release timestamp (default: current UTC time)
  --endpoint URL               PortalSurfer origin (production is exact)
  --source-ref REF             Require a non-detached checkout of REF
  --source-sha SHA             Require this exact source SHA
  --windows-release-dir DIR    Validated Windows nightly artifact directory
  --publisher-script PATH      Pinned Node publisher script for --publish

Production stable/RC remains the macOS arm64 CLAP + VST3 schema-2 path.
Production nightly additionally requires an unsigned Windows x86_64 VST3 ZIP
and emits one combined schema-3 manifest. Preflight is macOS-only and uses
ad-hoc signatures without Apple or PortalSurfer credentials.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --package-only|--publish|--preflight)
      [[ -z "${mode}" ]] || { echo "choose only one release mode" >&2; exit 2; }
      mode="${1#--}"; shift ;;
    --channel) channel="${2:?missing channel}"; shift 2 ;;
    --version) requested_version="${2:?missing version}"; shift 2 ;;
    --publication-version) requested_publication_version="${2:?missing publication version}"; shift 2 ;;
    --build-id) build_id="${2:?missing build id}"; shift 2 ;;
    --released-at) released_at="${2:?missing released-at}"; shift 2 ;;
    --endpoint) endpoint="${2:?missing endpoint}"; shift 2 ;;
    --source-ref) source_ref="${2:?missing source ref}"; shift 2 ;;
    --source-sha) requested_source_sha="${2:?missing source sha}"; shift 2 ;;
    --windows-release-dir) windows_release_dir="${2:?missing Windows release directory}"; shift 2 ;;
    --publisher-script) publisher_script="${2:?missing publisher script}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ -n "${mode}" ]] || { usage >&2; exit 2; }
[[ "${channel}" == stable || "${channel}" == rc || "${channel}" == nightly ]] || { echo "invalid channel: ${channel}" >&2; exit 2; }
if [[ -n "${publisher_script}" && "${mode}" != publish ]]; then
  echo "--publisher-script requires --publish" >&2
  exit 2
fi
if [[ "${mode}" == preflight || "${channel}" != nightly ]]; then
  [[ -z "${windows_release_dir}" ]] || { echo "--windows-release-dir is only valid for a production nightly" >&2; exit 2; }
else
  [[ -n "${windows_release_dir}" ]] || { echo "production WAVE nightly requires --windows-release-dir" >&2; exit 1; }
fi
[[ "$(uname -s)" == Darwin ]] || { echo "release packaging requires macOS" >&2; exit 1; }

if [[ -n "${source_ref}" ]]; then
  current_ref="$(git symbolic-ref --quiet --short HEAD || true)"
  [[ "${current_ref}" == "${source_ref}" ]] || { echo "requested source ${source_ref} does not match checkout ${current_ref:-detached}" >&2; exit 1; }
fi
[[ -z "$(git status --porcelain --untracked-files=all)" ]] || { echo "release source must be clean" >&2; git status --short >&2; exit 1; }

package_version="$(cargo metadata --locked --no-deps --format-version=1 --manifest-path "${repo_root}/Cargo.toml" | python3 -c 'import json, pathlib, sys
manifest_path = pathlib.Path(sys.argv[1]).resolve()
for package in json.load(sys.stdin)["packages"]:
    if pathlib.Path(package["manifest_path"]).resolve() == manifest_path:
        print(package["version"])
        break
else:
    raise SystemExit(f"package manifest not found: {manifest_path}")' "${repo_root}/Cargo.toml")"
[[ -n "${package_version}" ]] || { echo "Cargo.toml package version is missing" >&2; exit 1; }
if [[ -n "${requested_version}" && "${requested_version}" != "${package_version}" ]]; then
  echo "requested version ${requested_version} does not match Cargo.toml ${package_version}" >&2
  exit 1
fi
publication_version="${requested_publication_version:-${package_version}}"
python3 - "${package_version}" "${publication_version}" "${channel}" <<'PY'
import pathlib
import sys
sys.path.insert(0, str(pathlib.Path("scripts").resolve()))
from release_helper import validate_publication_version
validate_publication_version(sys.argv[1], sys.argv[2], sys.argv[3])
PY

source_sha="$(git rev-parse HEAD)"
[[ "${source_sha}" =~ ^[0-9a-f]{40}$ ]] || { echo "could not resolve an exact source SHA" >&2; exit 1; }
if [[ -n "${requested_source_sha}" ]]; then
  [[ "${requested_source_sha}" =~ ^[0-9a-f]{40}$ && "${requested_source_sha}" == "${source_sha}" ]] || { echo "requested source SHA does not match checkout HEAD" >&2; exit 1; }
fi
if [[ "${mode}" != preflight ]]; then
  current_ref="$(git symbolic-ref --quiet --short HEAD || true)"
  [[ "${current_ref}" == main ]] || { echo "production release source must be a non-detached main checkout" >&2; exit 1; }
  git fetch origin main --quiet
  canonical_main="$(git rev-parse refs/remotes/origin/main 2>/dev/null || true)"
  [[ -n "${canonical_main}" && "${source_sha}" == "${canonical_main}" ]] || { echo "production release source must equal origin/main (${canonical_main:-unavailable})" >&2; exit 1; }
fi
build_id="${build_id:-wave-v${publication_version}-${source_sha:0:12}}"
[[ "${build_id}" =~ ^[a-z0-9][a-z0-9._-]{1,127}$ ]] || { echo "invalid build id" >&2; exit 2; }
if [[ "${mode}" != preflight && "${channel}" == nightly ]]; then
  expected_build_id="wave-v${publication_version}-${source_sha:0:12}"
  [[ "${build_id}" == "${expected_build_id}" ]] || { echo "production nightly build id must be ${expected_build_id}" >&2; exit 1; }
fi
released_at="${released_at:-$(date -u '+%Y-%m-%dT%H:%M:%SZ')}"
[[ -s CHANGELOG.md ]] || { echo "CHANGELOG.md must not be empty" >&2; exit 1; }

if [[ "${mode}" == publish ]]; then
  [[ -n "${PORTALSURFER_RELEASE_TOKEN:-}" ]] || { echo "--publish requires PORTALSURFER_RELEASE_TOKEN (environment only)" >&2; exit 1; }
  [[ "${endpoint}" == "https://portalsurfer.org" ]] || { echo "production publishing requires exact origin https://portalsurfer.org" >&2; exit 1; }
  if [[ -n "${publisher_script}" ]]; then
    [[ -f "${publisher_script}" && ! -L "${publisher_script}" ]] || { echo "--publisher-script must point to a regular file" >&2; exit 1; }
  elif [[ "${channel}" == nightly ]]; then
    echo "production nightly publishing requires the pinned Node publisher script" >&2
    exit 1
  fi
fi
: "${VST3_SDK_DIR:?VST3_SDK_DIR must point to a VST3 SDK checkout}"
if [[ ! -d "${VST3_SDK_DIR:-}" ]]; then
  echo "VST3_SDK_DIR must point to the VST3 SDK checkout" >&2
  exit 1
fi
[[ -d "${VST3_SDK_DIR}/pluginterfaces" ]] || { echo "VST3_SDK_DIR must contain pluginterfaces/" >&2; exit 1; }
if [[ "${mode}" != preflight ]]; then
  for required in APPLE_DEVELOPER_ID_APPLICATION_CERT_BASE64 APPLE_DEVELOPER_ID_APPLICATION_CERT_PASSWORD APPLE_NOTARY_KEY_BASE64 APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER_ID; do
    [[ -n "${!required:-}" ]] || { echo "missing required Apple production credential: ${required}" >&2; exit 1; }
  done
fi

windows_release_root=""
windows_archive_name=""
if [[ "${mode}" != preflight && "${channel}" == nightly ]]; then
  windows_release_root="$(cd "${windows_release_dir}" 2>/dev/null && pwd -P)" || { echo "Windows release directory is not available: ${windows_release_dir}" >&2; exit 1; }
  python3 scripts/windows_release_helper.py validate --root "${windows_release_root}" --cargo-lock Cargo.lock --vst3-sdk-revision "${vst3_sdk_revision}"
  python3 - "${windows_release_root}" "${package_version}" "${publication_version}" "${build_id}" "${released_at}" "${source_sha}" <<'PY'
import json
import pathlib
import sys
root = pathlib.Path(sys.argv[1])
package_version, publication_version, build_id, released_at, source_sha = sys.argv[2:]
manifest = json.loads((root / "windows-artifact-manifest.json").read_text(encoding="utf-8"))
expected_archive = f"wave-v{publication_version}-windows-x86_64-unsigned.vst3.zip"
expected_build_id = f"wave-v{publication_version}-{source_sha[:12]}"
source = manifest.get("source")
if (manifest.get("schema_version") != 1 or manifest.get("product") != "wave" or manifest.get("format") != "vst3" or manifest.get("platform") != "windows" or manifest.get("architecture") != "x86_64" or manifest.get("package_version") != package_version or manifest.get("publication_version") != publication_version or manifest.get("channel") != "nightly" or manifest.get("build_id") != expected_build_id or manifest.get("build_id") != build_id or manifest.get("released_at") != released_at or not isinstance(source, dict) or source.get("git_sha") != source_sha or source.get("repository") != "PORTALSURFER/wave" or source.get("dirty") is not False or manifest.get("signing_status") != "unsigned" or manifest.get("signing_certificate") is not None or not isinstance(manifest.get("archive"), dict) or manifest["archive"].get("name") != expected_archive):
    raise SystemExit("Windows artifact manifest does not match the shared nightly identity")
expected_entries = {"windows-artifact-manifest.json", expected_archive}
entries = list(root.iterdir())
if {entry.name for entry in entries} != expected_entries or any(entry.is_symlink() or not entry.is_file() for entry in entries):
    raise SystemExit("Windows release directory must contain only the archive and its sidecar")
PY
  windows_archive_name="wave-v${publication_version}-windows-x86_64-unsigned.vst3.zip"
fi

release_dir="${repo_root}/dist/releases/${build_id}"
rm -rf -- "${release_dir}"
mkdir -p "${release_dir}"
mkdir -p "${repo_root}/target"
tmp_root="$(mktemp -d "${repo_root}/target/release-build.XXXXXX")"
evidence_dir="${tmp_root}/notary-evidence"
mkdir -p "${evidence_dir}"
original_keychains=()
release_keychain=""
original_keychains_file=""
cleanup() {
  if [[ -f "${original_keychains_file}" && "${#original_keychains[@]}" -gt 0 ]]; then
    security list-keychains -d user -s "${original_keychains[@]}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${release_keychain}" ]]; then security delete-keychain "${release_keychain}" >/dev/null 2>&1 || true; fi
  rm -rf -- "${tmp_root}"
}
trap cleanup EXIT

if [[ -n "${windows_archive_name}" ]]; then
  cp "${windows_release_root}/${windows_archive_name}" "${release_dir}/${windows_archive_name}"
fi

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
  while IFS= read -r keychain; do [[ -n "${keychain}" ]] && original_keychains+=("${keychain}"); done < "${original_keychains_file}"
  security create-keychain -p "${release_keychain_password}" "${release_keychain}" >/dev/null
  security set-keychain-settings -lut 21600 "${release_keychain}"
  security unlock-keychain -p "${release_keychain_password}" "${release_keychain}"
  security list-keychains -d user -s "${release_keychain}" "${original_keychains[@]}" >/dev/null
  security import "${cert_path}" -P "${APPLE_DEVELOPER_ID_APPLICATION_CERT_PASSWORD}" -A -t cert -f pkcs12 -k "${release_keychain}" >/dev/null
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "${release_keychain_password}" "${release_keychain}" >/dev/null
  codesign_identity="${APPLE_CODESIGN_IDENTITY:-}"
  if [[ -z "${codesign_identity}" ]]; then codesign_identity="$(security find-identity -v -p codesigning "${release_keychain}" | sed -n 's/.*"\(Developer ID Application:.*\)".*/\1/p' | head -n 1)"; fi
  [[ "${codesign_identity}" == Developer\ ID\ Application:* ]] || { echo "no Developer ID Application identity found" >&2; exit 1; }
fi

echo "[release] running VST3 gate"
bash scripts/ci.sh --vst3
echo "[release] rendering WAVE screenshots"
rm -rf -- target/ui-screenshots
bash scripts/ci.sh --screenshots
png="target/ui-screenshots/wave/initial-ui-default.png"
[[ -f "${png}" ]] || { echo "default screenshot was not produced" >&2; exit 1; }
cp "${png}" "${release_dir}/wave-default-960x600.png"

build_bundle() {
  local format="$1" target_zip="$2" bundle_dir="$3" binary="$4"
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
<key>CFBundleShortVersionString</key><string>${package_version}</string>
<key>CFBundleVersion</key><string>${package_version}</string>
</dict></plist>
EOF
  printf 'BNDL????' > "${contents}/PkgInfo"
  /usr/bin/plutil -lint "${contents}/Info.plist" >/dev/null
  if [[ "${mode}" == preflight ]]; then
    # validate_preflight_manifest uses require_production=False.
    printf 'preflight CodeResources\n' > "${contents}/CodeResources"
    codesign --force --deep --sign - "${bundle_dir}" >/dev/null
    codesign --verify --deep --strict "${bundle_dir}"
  else
    codesign --force --deep --timestamp --options runtime --keychain "${release_keychain}" --sign "${codesign_identity}" "${bundle_dir}" >/dev/null
    codesign --verify --deep --strict "${bundle_dir}"
    local notarize_zip="${tmp_root}/notary-${format}.zip"
    /usr/bin/ditto -c -k --sequesterRsrc --keepParent "${bundle_dir}" "${notarize_zip}"
    local notary_json="${tmp_root}/notary-${format}.json"
    xcrun notarytool submit "${notarize_zip}" --key "${notary_key_path}" --key-id "${APPLE_NOTARY_KEY_ID}" --issuer "${APPLE_NOTARY_ISSUER_ID}" --wait --output-format json > "${notary_json}"
    local notary_status notary_id
    notary_status="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("status", ""))' "${notary_json}")"
    notary_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id", ""))' "${notary_json}")"
    [[ "${notary_status}" == Accepted && -n "${notary_id}" ]] || { echo "notarization was not accepted for ${format}" >&2; cat "${notary_json}" >&2; exit 1; }
    notary_log="${evidence_dir}/notary-${format}-${notary_id}.json"
    xcrun notarytool log "${notary_id}" --key "${notary_key_path}" --key-id "${APPLE_NOTARY_KEY_ID}" --issuer "${APPLE_NOTARY_ISSUER_ID}" --output-format json > "${notary_log}"
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
  file "${contents}/MacOS/wave" | grep -q arm64 || { echo "${format} binary is not arm64" >&2; exit 1; }
  if command -v lipo >/dev/null 2>&1; then [[ "$(lipo -archs "${contents}/MacOS/wave")" == arm64 ]] || { echo "${format} binary must contain only arm64" >&2; exit 1; }; fi
  if [[ "${format}" == clap ]]; then /usr/bin/nm -gU "${contents}/MacOS/wave" | grep -q _clap_entry || { echo "CLAP entrypoint missing" >&2; exit 1; }; else
    for symbol in _GetPluginFactory _bundleEntry _bundleExit; do /usr/bin/nm -gU "${contents}/MacOS/wave" | grep -q "${symbol}" || { echo "VST3 symbol ${symbol} missing" >&2; exit 1; }; done
  fi
  /usr/bin/ditto -c -k --sequesterRsrc --keepParent "${bundle_dir}" "${target_zip}"
}

audit_zip() {
  local format="$1" archive="$2"
  python3 - "${archive}" "${format}" "${signing_team_id}" "${mode}" <<'PY'
import pathlib
import sys
sys.path.insert(0, str(pathlib.Path("scripts").resolve()))
from release_helper import _audit_zip
_audit_zip(pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3], cwd=pathlib.Path.cwd(), require_production=sys.argv[4] != "preflight")
PY
}

clap_target="${tmp_root}/clap-target"
vst3_target="${tmp_root}/vst3-target"
echo "[release] building CLAP"
TOYBOX_ACTIVE_ARTIFACT=clap CARGO_TARGET_DIR="${clap_target}" cargo build --locked --release
clap_binary="${clap_target}/release/libwave.dylib"
[[ -f "${clap_binary}" ]] || { echo "CLAP build did not produce ${clap_binary}" >&2; exit 1; }
build_bundle clap "${release_dir}/wave-v${publication_version}-macos.clap.zip" "${tmp_root}/wave.clap" "${clap_binary}"
audit_zip clap "${release_dir}/wave-v${publication_version}-macos.clap.zip"

echo "[release] building VST3"
TOYBOX_ACTIVE_ARTIFACT=vst3 VST3_SDK_DIR="${VST3_SDK_DIR}" CARGO_TARGET_DIR="${vst3_target}" cargo rustc --locked --release --features vst3 -- -C link-arg=-Wl,-bundle
vst3_binary="${vst3_target}/release/libwave.dylib"
[[ -f "${vst3_binary}" ]] || { echo "VST3 build did not produce ${vst3_binary}" >&2; exit 1; }
build_bundle vst3 "${release_dir}/wave-v${publication_version}-macos.vst3.zip" "${tmp_root}/wave.vst3" "${vst3_binary}"
audit_zip vst3 "${release_dir}/wave-v${publication_version}-macos.vst3.zip"
cp CHANGELOG.md "${release_dir}/CHANGELOG.md"

python3 - "${release_dir}" "${publication_version}" "${package_version}" "${build_id}" "${channel}" "${released_at}" "${source_sha}" "${mode}" "${signing_team_id}" "${clap_notary_id}" "${vst3_notary_id}" "${windows_archive_name}" <<'PY'
import pathlib
import sys
sys.path.insert(0, str(pathlib.Path("scripts").resolve()))
from release_helper import build_manifest, canonical_json, validate_preflight_manifest, validate_manifest
root, publication, package, build, channel, released_at, source, mode, team, clap_notary, vst3_notary, windows_name = sys.argv[1:]
folder = pathlib.Path(root)
kwargs = dict(publication_version=publication, package_version=package, build_id=build, channel=channel, released_at=released_at, git_sha=source, clap=folder / f"wave-v{publication}-macos.clap.zip", vst3=folder / f"wave-v{publication}-macos.vst3.zip", screenshot=folder / "wave-default-960x600.png", changelog=folder / "CHANGELOG.md")
if mode == "preflight":
    # validate_preflight_manifest enforces require_production=False.
    manifest = build_manifest(**kwargs, distribution="preflight", signing_identity_class="ad hoc", notarized=False, stapled=False)
    (folder / "release-manifest.json").write_bytes(canonical_json(manifest))
    validate_preflight_manifest(manifest, folder, package_version=package)
else:
    manifest = build_manifest(**kwargs, distribution="production", signing_identity_class="Developer ID Application", notarized=True, stapled=True, signing_team_id=team, notary_submissions={"clap": clap_notary, "vst3": vst3_notary}, windows_vst3=folder / windows_name if windows_name else None)
    (folder / "release-manifest.json").write_bytes(canonical_json(manifest))
    validate_manifest(manifest, folder)
PY

if [[ "${mode}" == publish ]]; then
  if [[ -n "${publisher_script}" ]]; then
    node "${publisher_script}" --manifest "${release_dir}/release-manifest.json" --root "${release_dir}" --endpoint "${endpoint}"
  else
    python3 - "${release_dir}" "${endpoint}" "${package_version}" <<'PY'
import os
import pathlib
import sys
sys.path.insert(0, str(pathlib.Path("scripts").resolve()))
from release_helper import publish_release
root = pathlib.Path(sys.argv[1])
publish_release(endpoint=sys.argv[2], token=os.environ.get("PORTALSURFER_RELEASE_TOKEN", ""), manifest_path=root / "release-manifest.json", root=root, repo_root=pathlib.Path.cwd(), package_version=sys.argv[3])
PY
  fi
fi

echo "[release] bundle ready: ${release_dir}"
find "${release_dir}" -maxdepth 1 -type f -print | sort
