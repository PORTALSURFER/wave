#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

if command -v git-cliff >/dev/null 2>&1; then
  git_cliff_bin="git-cliff"
elif [[ -x "${HOME}/.cargo/bin/git-cliff" ]]; then
  git_cliff_bin="${HOME}/.cargo/bin/git-cliff"
else
  echo "[changelog] git-cliff is not installed or not on PATH" >&2
  echo "[changelog] install with: cargo install git-cliff" >&2
  exit 1
fi

"${git_cliff_bin}" --config .git-cliff.toml --output CHANGELOG.md

echo "[changelog] updated CHANGELOG.md"
