#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
audiodev_root="$(cd "${repo_root}/.." && pwd)"
dist_dir="${DIST_DIR:-${audiodev_root}/dist}"
version="$(sed -n 's/^[[:space:]]*version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1)"
bundle_name="wave-v${version}-macos.clap"
destination="${dist_dir}/${bundle_name}"

[[ "$(uname -s)" == Darwin ]] || { printf 'macOS is required\n' >&2; exit 1; }
: "${VST3_SDK_DIR:?VST3_SDK_DIR must point to a VST3 SDK checkout}"
[[ -d "${VST3_SDK_DIR}/pluginterfaces" ]] || { printf 'VST3 SDK is missing pluginterfaces/\n' >&2; exit 1; }

cd "${repo_root}"
cargo build --locked --release
binary="${repo_root}/target/release/libwave.dylib"
[[ -f "${binary}" ]] || { printf 'missing CLAP binary: %s\n' "${binary}" >&2; exit 1; }

staging_root="$(mktemp -d "${TMPDIR:-/tmp}/wave-clap.XXXXXX")"
staged="${staging_root}/${bundle_name}"
trap 'rm -rf "${staging_root}"' EXIT
mkdir -p "${staged}/Contents/MacOS"
cp "${binary}" "${staged}/Contents/MacOS/wave"
chmod 755 "${staged}/Contents/MacOS/wave"
cp resources/wave-clap-Info.plist "${staged}/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "${version}" "${staged}/Contents/Info.plist"
plutil -replace CFBundleVersion -string "${version}" "${staged}/Contents/Info.plist"
printf 'BNDL????' > "${staged}/Contents/PkgInfo"
plutil -lint "${staged}/Contents/Info.plist" >/dev/null
codesign --force --deep --sign - "${staged}"
codesign --verify --deep --strict "${staged}"
/usr/bin/nm -gU "${staged}/Contents/MacOS/wave" | grep -q '_clap_entry'

mkdir -p "${dist_dir}"
rm -rf "${destination}"
mv "${staged}" "${destination}"
printf 'wrote %s\n' "${destination}"
/usr/bin/shasum -a 256 "${destination}/Contents/MacOS/wave"

DIST_DIR="${dist_dir}" VST3_SDK_DIR="${VST3_SDK_DIR}" \
  bash "${audiodev_root}/scripts/build-vst3-release.sh" wave
