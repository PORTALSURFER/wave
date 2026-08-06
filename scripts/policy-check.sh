#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

fail() {
  printf '[policy] %s\n' "$*" >&2
  exit 1
}

manifest=Cargo.toml
[[ -f "${manifest}" ]] || fail "Cargo.toml is missing"

rg -n '^\[patch(\.|\])' "${manifest}" && fail "[patch] overrides are not allowed"
rg -n 'branch[[:space:]]*=' "${manifest}" && fail "branch pins are not allowed"
rg -n '^(clack-(plugin|extensions|common|host)|clap-sys|baseview)[[:space:]]*=' "${manifest}" \
  && fail "use Toybox re-exports instead of direct host crates"

toybox_line="$(rg '^toybox[[:space:]]*=' "${manifest}")"
[[ "${toybox_line}" =~ git[[:space:]]*=[[:space:]]*\"https://github.com/PORTALSURFER/toybox\.git\" ]] \
  || fail "Toybox must use the canonical git URL"
[[ "${toybox_line}" =~ rev[[:space:]]*=[[:space:]]*\"[0-9a-fA-F]{40}\" ]] \
  || fail "Toybox must use a full 40-character revision"

radiant_line="$(rg '^radiant[[:space:]]*=' "${manifest}")"
[[ "${radiant_line}" =~ git[[:space:]]*=[[:space:]]*\"https://github.com/PORTALSURFER/radiant\.git\" ]] \
  || fail "Radiant must use the canonical git URL"
[[ "${radiant_line}" =~ rev[[:space:]]*=[[:space:]]*\"[0-9a-fA-F]{40}\" ]] \
  || fail "Radiant must use a full 40-character revision"

[[ ! -f .cargo/config.toml ]] || fail ".cargo/config.toml must remain local-only"
rg -n 'screenshot_renders_initial_ui' src --glob '*.rs' >/dev/null \
  || fail "the retained editor needs screenshot coverage"
rg -n '^screenshot-test[[:space:]]*=' "${manifest}" >/dev/null \
  || fail "screenshot-test feature is required"

printf '[policy] ok\n'
