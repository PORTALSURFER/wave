#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

want_vst3=0
want_screenshots=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --vst3) want_vst3=1; shift ;;
    --screenshots) want_screenshots=1; shift ;;
    -h|--help)
      printf 'Usage: scripts/ci.sh [--vst3] [--screenshots]\n'
      exit 0
      ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

[[ "${want_vst3}" == 0 || "${want_screenshots}" == 0 ]] \
  || { printf '%s\n' '--vst3 and --screenshots are separate gates' >&2; exit 2; }

bash scripts/policy-check.sh
cargo fmt --all -- --check

if [[ "${want_vst3}" == 1 ]]; then
  : "${VST3_SDK_DIR:?VST3_SDK_DIR must point to a VST3 SDK checkout}"
  [[ -d "${VST3_SDK_DIR}/pluginterfaces" ]] \
    || { printf 'VST3_SDK_DIR/pluginterfaces is missing\n' >&2; exit 2; }
  cargo clippy --locked --all-targets --features vst3 -- -D warnings
  cargo test --locked --all --features vst3
  exit 0
fi

cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all

if [[ "${want_screenshots}" == 1 ]]; then
  mkdir -p target/ui-screenshots
  TOYBOX_UI_SCREENSHOT=1 \
    TOYBOX_UI_SCREENSHOT_DIR=target/ui-screenshots \
    cargo test --locked --release --features screenshot-test gui::screenshot_tests -- --nocapture
  [[ -f target/ui-screenshots/wave/initial-ui-default.png ]] \
    || { printf 'screenshot artifact was not produced\n' >&2; exit 1; }
  [[ -f target/ui-screenshots/wave/high-resolution-waveform.png ]] \
    || { printf 'high-resolution waveform screenshot artifact was not produced\n' >&2; exit 1; }
  [[ -f target/ui-screenshots/wave/partial-live-waveform.png ]] \
    || { printf 'partial-live waveform screenshot artifact was not produced\n' >&2; exit 1; }
  [[ -f target/ui-screenshots/wave/open-window-dropdown.png ]] \
    || { printf 'open-window dropdown screenshot artifact was not produced\n' >&2; exit 1; }
  [[ -f target/ui-screenshots/wave/multi-beat-live-waveform.png ]] \
    || { printf 'multi-beat live waveform screenshot artifact was not produced\n' >&2; exit 1; }
fi
