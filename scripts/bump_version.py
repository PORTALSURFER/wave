#!/usr/bin/env python3
"""Update WAVE's package and lockfile version for one release bump."""

from __future__ import annotations

import re
import sys
from pathlib import Path

CORE_SEMVER = re.compile(r"([0-9]+)\.([0-9]+)\.([0-9]+)\Z")
PACKAGE_VERSION = re.compile(r'(?m)^(version\s*=\s*")[^"]+("\s*)$')
LOCK_PACKAGE = re.compile(r'(?ms)(^\[\[package\]\]\nname = "wave"\nversion = ")[^"]+("\n)')


def _parse_version(version: str) -> tuple[int, int, int]:
    match = CORE_SEMVER.fullmatch(version)
    if match is None:
        raise ValueError(f"version must be numeric semver: {version}")
    return tuple(int(part) for part in match.groups())


def bump_package_version(manifest_path: Path, lock_path: Path, new_version: str) -> str:
    """Advance the package to the selected release version."""
    target = _parse_version(new_version)
    manifest_text = manifest_path.read_text(encoding="utf-8")
    package_start = manifest_text.find("[package]")
    if package_start < 0:
        raise ValueError("Cargo.toml package table is missing")
    next_table = manifest_text.find("\n[", package_start + len("[package]"))
    package_end = len(manifest_text) if next_table < 0 else next_table
    package_text = manifest_text[package_start:package_end]
    current_match = PACKAGE_VERSION.search(package_text)
    if current_match is None:
        raise ValueError("Cargo.toml package version line is missing or ambiguous")
    current_version = current_match.group(0).split('"', 1)[1].rsplit('"', 1)[0]
    current = _parse_version(current_version)
    if target <= current:
        raise ValueError(f"release version must be newer than {current_version}")

    updated_package, manifest_matches = PACKAGE_VERSION.subn(rf'\g<1>{new_version}\g<2>', package_text, count=1)
    if manifest_matches != 1:
        raise ValueError("Cargo.toml package version line is missing or ambiguous")

    lock_text = lock_path.read_text(encoding="utf-8")
    updated_lock, lock_matches = LOCK_PACKAGE.subn(rf'\g<1>{new_version}\g<2>', lock_text, count=1)
    if lock_matches != 1:
        raise ValueError("Cargo.lock WAVE package entry is missing or ambiguous")

    manifest_path.write_text(manifest_text[:package_start] + updated_package + manifest_text[package_end:], encoding="utf-8")
    lock_path.write_text(updated_lock, encoding="utf-8")
    return current_version


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {Path(sys.argv[0]).name} VERSION", file=sys.stderr)
        return 2
    try:
        previous = bump_package_version(Path("Cargo.toml"), Path("Cargo.lock"), sys.argv[1])
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"bumped WAVE from v{previous} to v{sys.argv[1]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
