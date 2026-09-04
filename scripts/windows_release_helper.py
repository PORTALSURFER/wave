#!/usr/bin/env python3
"""Build and validate WAVE's unsigned Windows x86_64 VST3 artifact."""

from __future__ import annotations

import argparse
import ast
import datetime as dt
import hashlib
import json
import re
import shutil
import struct
import sys
import tempfile
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any, Mapping


SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from release_helper import canonical_json, file_digest, validate_publication_version


MANIFEST_SCHEMA = 1
PRODUCT = "wave"
REPOSITORY = "PORTALSURFER/wave"
FORMAT = "vst3"
PLATFORM = "windows"
ARCHITECTURE = "x86_64"
SIGNING_STATUS = "unsigned"
SIGNING_CERTIFICATE = None
VST3_SDK_REPOSITORY = "https://github.com/steinbergmedia/vst3sdk.git"
VST3_SDK_REVISION = "58f8da7936800732561402d7936584ca4505de07"
DEPENDENCY_REPOSITORIES = {
    "toybox": "https://github.com/PORTALSURFER/toybox.git",
    "radiant": "https://github.com/PORTALSURFER/radiant.git",
    "vst3sdk": VST3_SDK_REPOSITORY,
}
REQUIRED_DEPENDENCIES = frozenset(DEPENDENCY_REPOSITORIES)
SAFE_BUILD_ID = re.compile(r"[a-z0-9][a-z0-9._-]{1,127}\Z")
SHA1 = re.compile(r"[0-9a-f]{40}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
SEMVER = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?\Z")
SAFE_ARCHIVE_NAME = re.compile(r"wave-v[0-9A-Za-z.-]+-windows-x86_64-unsigned\.vst3\.zip\Z")
MAX_ARCHIVE_MEMBER_BYTES = 512 * 1024 * 1024
PE32_PLUS_MAGIC = 0x20B
IMAGE_FILE_MACHINE_AMD64 = 0x8664
PE_DATA_DIRECTORY_OFFSET = 112
IMAGE_DIRECTORY_ENTRY_SECURITY = 4
WINDOWS_PATH_POLICY = "relative-no-traversal-no-symlinks"
WINDOWS_RUNNER_IMAGE = "windows-2022"
WINDOWS_RUNNER_OS = "win22"
RUST_TOOLCHAIN = "1.97.1"
RUST_TARGET = "x86_64-pc-windows-msvc"
PYTHON_IMPLEMENTATION = "CPython"
WINDOWS_RELEASE_CHANNEL = "nightly"


def _positive_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value > 0


def _validate_build_environment(build_environment: Mapping[str, Any]) -> dict[str, dict[str, str]]:
    if not isinstance(build_environment, Mapping) or set(build_environment) != {"runner", "rust", "python"}:
        raise ValueError("Windows build environment metadata is invalid")
    runner = build_environment["runner"]
    if not isinstance(runner, Mapping) or set(runner) != {"image", "image_os", "image_version"}:
        raise ValueError("Windows runner provenance is invalid")
    if runner.get("image") != WINDOWS_RUNNER_IMAGE or runner.get("image_os") != WINDOWS_RUNNER_OS or not isinstance(runner.get("image_version"), str) or re.fullmatch(r"[0-9]{8}\.[0-9]+\.[0-9]+", runner["image_version"]) is None:
        raise ValueError("Windows runner provenance does not match the pinned image")
    rust = build_environment["rust"]
    if not isinstance(rust, Mapping) or set(rust) != {"toolchain", "target", "rustc_version"}:
        raise ValueError("Rust compiler provenance is invalid")
    if rust.get("toolchain") != RUST_TOOLCHAIN or rust.get("target") != RUST_TARGET or not isinstance(rust.get("rustc_version"), str) or re.fullmatch(rf"rustc {re.escape(RUST_TOOLCHAIN)} \([^()\r\n]+\)", rust["rustc_version"]) is None:
        raise ValueError("Rust compiler provenance does not match the pinned toolchain")
    python = build_environment["python"]
    if not isinstance(python, Mapping) or set(python) != {"implementation", "version"}:
        raise ValueError("Python provenance is invalid")
    if python.get("implementation") != PYTHON_IMPLEMENTATION or not isinstance(python.get("version"), str) or re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", python["version"]) is None:
        raise ValueError("Python provenance is invalid")
    return {
        "runner": {"image": runner["image"], "image_os": runner["image_os"], "image_version": runner["image_version"]},
        "rust": {"toolchain": rust["toolchain"], "target": rust["target"], "rustc_version": rust["rustc_version"]},
        "python": {"implementation": python["implementation"], "version": python["version"]},
    }


def bundle_name(package_version: str) -> str:
    return f"WAVE-v{package_version}.vst3"


def archive_name(publication_version: str) -> str:
    return f"wave-v{publication_version}-windows-x86_64-unsigned.vst3.zip"


def bundle_member_name(package_version: str) -> str:
    name = bundle_name(package_version)
    return f"{name}/Contents/x86_64-win/{name}"


def source_binary_suffix(package_version: str) -> tuple[str, ...]:
    name = bundle_name(package_version)
    return ("dist", name, "Contents", "x86_64-win", name)


def _validate_regular_file(path: Path, label: str) -> None:
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"{label} must be a regular file: {path}")


def _validate_identity(*, package_version: str, publication_version: str, channel: str, build_id: str, released_at: str, source_sha: str) -> None:
    if channel != WINDOWS_RELEASE_CHANNEL:
        raise ValueError("Windows artifacts support only the nightly channel")
    if not isinstance(package_version, str) or SEMVER.fullmatch(package_version) is None or "-" in package_version:
        raise ValueError("package version must be a numeric semver")
    try:
        validate_publication_version(package_version, publication_version, channel)
    except ValueError as error:
        raise ValueError(f"invalid publication version: {error}") from error
    if not isinstance(build_id, str) or SAFE_BUILD_ID.fullmatch(build_id) is None:
        raise ValueError("source build id is invalid")
    if not isinstance(source_sha, str) or SHA1.fullmatch(source_sha) is None:
        raise ValueError("source revision must be a lowercase 40-character SHA")
    if not isinstance(released_at, str):
        raise ValueError("released_at must be RFC3339")
    try:
        parsed = dt.datetime.fromisoformat(released_at.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError("released_at must be RFC3339") from error
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise ValueError("released_at must include a timezone")


def _validate_pe_bytes(data: bytes, label: str) -> None:
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValueError(f"{label} is not a PE executable")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if pe_offset + 26 > len(data) or data[pe_offset : pe_offset + 4] != b"PE\x00\x00":
        raise ValueError(f"{label} has an invalid PE header")
    machine = struct.unpack_from("<H", data, pe_offset + 4)[0]
    optional_size = struct.unpack_from("<H", data, pe_offset + 20)[0]
    optional_offset = pe_offset + 24
    if optional_offset + 2 > len(data):
        raise ValueError(f"{label} has an invalid PE optional header")
    optional_magic = struct.unpack_from("<H", data, optional_offset)[0]
    if machine != IMAGE_FILE_MACHINE_AMD64 or optional_magic != PE32_PLUS_MAGIC:
        raise ValueError(f"{label} must be an x86_64 PE32+ binary")
    if optional_size < PE_DATA_DIRECTORY_OFFSET + 4 or optional_offset + optional_size > len(data):
        raise ValueError(f"{label} has an invalid PE optional header")
    number_of_directories = struct.unpack_from("<I", data, optional_offset + 108)[0]
    if number_of_directories <= IMAGE_DIRECTORY_ENTRY_SECURITY:
        return
    security_directory_offset = optional_offset + PE_DATA_DIRECTORY_OFFSET + IMAGE_DIRECTORY_ENTRY_SECURITY * 8
    if security_directory_offset + 8 > optional_offset + optional_size:
        raise ValueError(f"{label} has an invalid PE data directory")
    certificate_offset, certificate_size = struct.unpack_from("<II", data, security_directory_offset)
    if certificate_offset or certificate_size:
        raise ValueError(f"{label} contains an Authenticode certificate; unsigned artifact required")


def validate_pe_binary(binary: Path) -> tuple[str, int]:
    _validate_regular_file(binary, "Windows VST3 binary")
    data = binary.read_bytes()
    if not data:
        raise ValueError("Windows VST3 binary is empty")
    _validate_pe_bytes(data, "Windows VST3 binary")
    return file_digest(binary)


def _validate_source_binary_path(binary: Path, package_version: str) -> None:
    expected_suffix = source_binary_suffix(package_version)
    if tuple(binary.parts[-len(expected_suffix) :]) != expected_suffix:
        raise ValueError(f"Windows VST3 build output must be under {Path(*expected_suffix)}")


def _validate_zip_member_path(info: zipfile.ZipInfo) -> None:
    name = info.filename
    if not name or "\\" in name or "\x00" in name or name.startswith(("./", "/", "\\")) or name.endswith("/") or re.match(r"^[A-Za-z]:", name):
        raise ValueError(f"archive member path is unsafe: {name!r}")
    if not PurePosixPath(name).parts or any(part in {"", ".", ".."} for part in name.split("/")):
        raise ValueError(f"archive member path is unsafe: {name!r}")
    if info.is_dir() or info.flag_bits & 0x1:
        raise ValueError("archive must contain one unencrypted regular file")
    if info.compress_type not in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED}:
        raise ValueError("archive uses an unsupported compression method")
    if info.file_size <= 0 or info.file_size > MAX_ARCHIVE_MEMBER_BYTES:
        raise ValueError("archive member size is outside the allowed range")
    mode = (info.external_attr >> 16) & 0o170000
    if mode not in {0, 0o100000}:
        raise ValueError("archive member must be a regular file, not a link or special file")


def validate_archive(archive: Path, *, package_version: str, publication_version: str) -> dict[str, Any]:
    expected_name = archive_name(publication_version)
    expected_member = bundle_member_name(package_version)
    if archive.name != expected_name or SAFE_ARCHIVE_NAME.fullmatch(archive.name) is None:
        raise ValueError(f"Windows artifact must be named {expected_name}")
    _validate_regular_file(archive, "Windows artifact archive")
    try:
        with zipfile.ZipFile(archive, "r") as zip_file:
            members = zip_file.infolist()
            if len(members) != 1:
                raise ValueError("Windows VST3 archive must contain exactly one file")
            info = members[0]
            _validate_zip_member_path(info)
            if info.filename != expected_member:
                raise ValueError(f"Windows VST3 archive layout must contain {expected_member}")
            data = zip_file.read(info)
            if len(data) != info.file_size:
                raise ValueError("archive member size does not match its ZIP metadata")
            _validate_pe_bytes(data, "Windows VST3 archive member")
    except zipfile.BadZipFile as error:
        raise ValueError("Windows VST3 artifact is not a valid ZIP archive") from error
    except RuntimeError as error:
        raise ValueError(f"could not read Windows VST3 archive: {error}") from error
    archive_hash, archive_size = file_digest(archive)
    member_hash = hashlib.sha256(data).hexdigest()
    return {"name": archive.name, "sha256": archive_hash, "size_bytes": archive_size, "bundle_name": bundle_name(package_version), "member_name": expected_member, "member_sha256": member_hash, "member_size_bytes": len(data)}


def _parse_git_source(source: Any, package_name: str) -> dict[str, str]:
    if not isinstance(source, str) or not source.startswith("git+"):
        raise ValueError(f"{package_name} dependency is not pinned to a git revision")
    value = source[len("git+") :]
    base, separator, revision = value.rpartition("#")
    if not separator or SHA1.fullmatch(revision) is None:
        raise ValueError(f"{package_name} dependency has an invalid git revision")
    repository, query_separator, query = base.partition("?")
    if query_separator:
        query_values = dict(item.split("=", 1) for item in query.split("&") if "=" in item)
        if query_values.get("rev") not in (None, revision):
            raise ValueError(f"{package_name} dependency git query and revision disagree")
    if repository != DEPENDENCY_REPOSITORIES[package_name]:
        raise ValueError(f"{package_name} dependency repository is not the expected dependency")
    return {"repository": repository, "revision": revision}


def _parse_lock_packages_without_tomllib(contents: str) -> list[dict[str, Any]]:
    packages: list[dict[str, Any]] = []
    current: dict[str, Any] | None = None
    for raw_line in contents.splitlines() + ["[[end]]"]:
        line = raw_line.strip()
        if line == "[[package]]":
            if current is not None:
                packages.append(current)
            current = {}
        elif line.startswith("[["):
            if current is not None:
                packages.append(current)
            current = None
        elif current is not None and "=" in line:
            key, raw_value = (part.strip() for part in line.split("=", 1))
            if key not in {"name", "source"}:
                continue
            try:
                value = ast.literal_eval(raw_value)
            except (SyntaxError, ValueError) as error:
                raise ValueError(f"Cargo.lock contains an invalid {key} value") from error
            if not isinstance(value, str):
                raise ValueError(f"Cargo.lock {key} value must be a string")
            current[key] = value
    return packages


def load_dependency_revisions(lockfile: Path) -> dict[str, dict[str, str]]:
    try:
        contents = lockfile.read_text(encoding="utf-8")
    except OSError as error:
        raise ValueError(f"could not read Cargo.lock: {error}") from error
    try:
        import tomllib
    except ModuleNotFoundError:
        tomllib = None
    if tomllib is not None:
        try:
            packages = tomllib.loads(contents).get("package")
        except tomllib.TOMLDecodeError as error:
            raise ValueError(f"could not read Cargo.lock: {error}") from error
    else:
        packages = _parse_lock_packages_without_tomllib(contents)
    if not isinstance(packages, list):
        raise ValueError("Cargo.lock does not contain package entries")
    dependencies: dict[str, dict[str, str]] = {}
    for name in ("toybox", "radiant"):
        candidates = [package for package in packages if isinstance(package, dict) and package.get("name") == name]
        if len(candidates) != 1:
            raise ValueError(f"Cargo.lock must contain exactly one {name} package")
        dependencies[name] = _parse_git_source(candidates[0].get("source"), name)
    return dependencies


def _validate_dependencies(dependencies: Mapping[str, Any]) -> dict[str, dict[str, str]]:
    if not isinstance(dependencies, Mapping) or set(dependencies) != REQUIRED_DEPENDENCIES:
        raise ValueError("manifest dependency revisions are incomplete or contain unexpected entries")
    normalized: dict[str, dict[str, str]] = {}
    for name in sorted(REQUIRED_DEPENDENCIES):
        value = dependencies[name]
        if not isinstance(value, dict) or set(value) != {"repository", "revision"} or value["repository"] != DEPENDENCY_REPOSITORIES[name] or not isinstance(value["revision"], str) or SHA1.fullmatch(value["revision"]) is None:
            raise ValueError(f"manifest dependency revision is invalid: {name}")
        normalized[name] = {"repository": value["repository"], "revision": value["revision"]}
    return normalized


def dependency_revisions(lockfile: Path, *, vst3_sdk_revision: str = VST3_SDK_REVISION) -> dict[str, dict[str, str]]:
    if not isinstance(vst3_sdk_revision, str) or SHA1.fullmatch(vst3_sdk_revision) is None:
        raise ValueError("VST3 SDK revision must be a lowercase 40-character SHA")
    dependencies = load_dependency_revisions(lockfile)
    dependencies["vst3sdk"] = {"repository": VST3_SDK_REPOSITORY, "revision": vst3_sdk_revision}
    return _validate_dependencies(dependencies)


def build_manifest(*, package_version: str, publication_version: str, channel: str, build_id: str, released_at: str, source_sha: str, archive: Path, dependencies: Mapping[str, Any], build_environment: Mapping[str, Any]) -> dict[str, Any]:
    _validate_identity(package_version=package_version, publication_version=publication_version, channel=channel, build_id=build_id, released_at=released_at, source_sha=source_sha)
    details = validate_archive(archive, package_version=package_version, publication_version=publication_version)
    return {
        "schema_version": MANIFEST_SCHEMA,
        "product": PRODUCT,
        "format": FORMAT,
        "platform": PLATFORM,
        "architecture": ARCHITECTURE,
        "package_version": package_version,
        "publication_version": publication_version,
        "channel": channel,
        "build_id": build_id,
        "released_at": released_at,
        "build_environment": _validate_build_environment(build_environment),
        "source": {"repository": REPOSITORY, "git_sha": source_sha, "dirty": False},
        "dependencies": _validate_dependencies(dependencies),
        "signing_status": SIGNING_STATUS,
        "signing_certificate": SIGNING_CERTIFICATE,
        "archive": {
            "name": details["name"],
            "media_type": "application/zip",
            "sha256": details["sha256"],
            "size_bytes": details["size_bytes"],
            "member_count": 1,
            "path_policy": WINDOWS_PATH_POLICY,
            "layout": {"bundle": details["bundle_name"], "binary": details["member_name"]},
            "members": [{"path": details["member_name"], "sha256": details["member_sha256"], "size_bytes": details["member_size_bytes"]}],
        },
    }


def validate_manifest(manifest: Mapping[str, Any], root: Path, *, cargo_lock: Path | None = None, vst3_sdk_revision: str = VST3_SDK_REVISION) -> None:
    required = {"schema_version", "product", "format", "platform", "architecture", "package_version", "publication_version", "channel", "build_id", "released_at", "build_environment", "source", "dependencies", "signing_status", "signing_certificate", "archive"}
    if not isinstance(manifest, Mapping) or set(manifest) != required or type(manifest.get("schema_version")) is not int or manifest.get("schema_version") != MANIFEST_SCHEMA:
        raise ValueError("Windows artifact manifest schema or fields are invalid")
    if manifest.get("product") != PRODUCT or manifest.get("format") != FORMAT or manifest.get("platform") != PLATFORM or manifest.get("architecture") != ARCHITECTURE:
        raise ValueError("Windows artifact manifest identity is invalid")
    source = manifest["source"]
    if not isinstance(source, dict) or set(source) != {"repository", "git_sha", "dirty"} or source.get("repository") != REPOSITORY or source.get("dirty") is not False:
        raise ValueError("Windows artifact source metadata is invalid")
    _validate_identity(package_version=manifest["package_version"], publication_version=manifest["publication_version"], channel=manifest["channel"], build_id=manifest["build_id"], released_at=manifest["released_at"], source_sha=source["git_sha"])
    _validate_build_environment(manifest["build_environment"])
    if manifest["signing_status"] != SIGNING_STATUS or manifest["signing_certificate"] is not None:
        raise ValueError("Windows artifact must explicitly be unsigned with no certificate")
    dependencies = _validate_dependencies(manifest["dependencies"])
    if cargo_lock is not None and dependencies != dependency_revisions(cargo_lock, vst3_sdk_revision=vst3_sdk_revision):
        raise ValueError("manifest dependency revisions do not match Cargo.lock and the pinned VST3 SDK")
    archive = manifest["archive"]
    fields = {"name", "media_type", "sha256", "size_bytes", "member_count", "path_policy", "layout", "members"}
    if not isinstance(archive, dict) or set(archive) != fields or archive["media_type"] != "application/zip" or type(archive["member_count"]) is not int or archive["member_count"] != 1 or archive["path_policy"] != WINDOWS_PATH_POLICY:
        raise ValueError("Windows artifact archive metadata is invalid")
    expected_archive = root / archive_name(manifest["publication_version"])
    if archive["name"] != expected_archive.name or not isinstance(archive["sha256"], str) or not SHA256.fullmatch(archive["sha256"]) or not _positive_int(archive["size_bytes"]):
        raise ValueError("Windows artifact archive identity or hash is invalid")
    expected_bundle = bundle_name(manifest["package_version"])
    expected_member = bundle_member_name(manifest["package_version"])
    if archive.get("layout") != {"bundle": expected_bundle, "binary": expected_member}:
        raise ValueError("Windows artifact archive layout metadata is invalid")
    members = archive["members"]
    if not isinstance(members, list) or len(members) != 1 or not isinstance(members[0], dict) or set(members[0]) != {"path", "sha256", "size_bytes"} or members[0]["path"] != expected_member or not isinstance(members[0]["sha256"], str) or not SHA256.fullmatch(members[0]["sha256"]) or not _positive_int(members[0]["size_bytes"]):
        raise ValueError("Windows artifact archive member metadata is invalid")
    details = validate_archive(expected_archive, package_version=manifest["package_version"], publication_version=manifest["publication_version"])
    if details["sha256"] != archive["sha256"] or details["size_bytes"] != archive["size_bytes"] or details["member_sha256"] != members[0]["sha256"] or details["member_size_bytes"] != members[0]["size_bytes"]:
        raise ValueError("Windows artifact archive hash or size mismatch")
    manifest_path = root / "windows-artifact-manifest.json"
    if manifest_path.exists():
        _validate_regular_file(manifest_path, "Windows artifact manifest")
        if manifest_path.read_bytes() != canonical_json(dict(manifest)):
            raise ValueError("windows-artifact-manifest.json is not canonical JSON")


def package_windows_vst3(*, binary: Path, output_dir: Path, package_version: str, publication_version: str, channel: str, build_id: str, released_at: str, source_sha: str, dependencies: Mapping[str, Any], build_environment: Mapping[str, Any]) -> dict[str, Any]:
    _validate_identity(package_version=package_version, publication_version=publication_version, channel=channel, build_id=build_id, released_at=released_at, source_sha=source_sha)
    _validate_source_binary_path(binary, package_version)
    validate_pe_binary(binary)
    output_dir.mkdir(parents=True, exist_ok=True)
    archive_path = output_dir / archive_name(publication_version)
    manifest_path = output_dir / "windows-artifact-manifest.json"
    if archive_path.exists() or manifest_path.exists():
        raise ValueError(f"refusing to overwrite existing Windows release output in {output_dir}")
    member = bundle_member_name(package_version)
    with tempfile.TemporaryDirectory(prefix="windows-vst3-", dir=output_dir) as temporary:
        temporary_root = Path(temporary)
        staged_binary = temporary_root / member
        staged_binary.parent.mkdir(parents=True)
        shutil.copyfile(binary, staged_binary)
        staged_archive = temporary_root / archive_path.name
        data = staged_binary.read_bytes()
        info = zipfile.ZipInfo(member)
        info.date_time = (1980, 1, 1, 0, 0, 0)
        info.compress_type = zipfile.ZIP_DEFLATED
        info.create_system = 0
        info.external_attr = 0
        with zipfile.ZipFile(staged_archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zip_file:
            zip_file.writestr(info, data)
        manifest = build_manifest(package_version=package_version, publication_version=publication_version, channel=channel, build_id=build_id, released_at=released_at, source_sha=source_sha, archive=staged_archive, dependencies=dependencies, build_environment=build_environment)
        staged_manifest = temporary_root / manifest_path.name
        staged_manifest.write_bytes(canonical_json(manifest))
        validate_manifest(manifest, temporary_root)
        staged_archive.replace(archive_path)
        staged_manifest.replace(manifest_path)
    return manifest


def _package_command(args: argparse.Namespace) -> None:
    manifest = package_windows_vst3(binary=Path(args.binary), output_dir=Path(args.output_dir), package_version=args.package_version, publication_version=args.publication_version, channel=args.channel, build_id=args.build_id, released_at=args.released_at, source_sha=args.source_sha, dependencies=dependency_revisions(Path(args.cargo_lock), vst3_sdk_revision=args.vst3_sdk_revision), build_environment={"runner": {"image": args.runner_image, "image_os": args.runner_image_os, "image_version": args.runner_image_version}, "rust": {"toolchain": args.rust_toolchain, "target": args.rust_target, "rustc_version": args.rustc_version}, "python": {"implementation": args.python_implementation, "version": args.python_version}})
    print(canonical_json(manifest).decode("utf-8"), end="")


def _validate_command(args: argparse.Namespace) -> None:
    root = Path(args.root)
    manifest_path = root / "windows-artifact-manifest.json"
    _validate_regular_file(manifest_path, "Windows artifact manifest")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"could not read Windows artifact manifest: {error}") from error
    validate_manifest(manifest, root, cargo_lock=Path(args.cargo_lock), vst3_sdk_revision=args.vst3_sdk_revision)
    print(f"validated {manifest_path}")


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    package = subparsers.add_parser("package", help="package and validate an unsigned Windows VST3")
    package.add_argument("--binary", required=True)
    package.add_argument("--output-dir", required=True)
    package.add_argument("--package-version", required=True)
    package.add_argument("--publication-version", required=True)
    package.add_argument("--channel", choices=(WINDOWS_RELEASE_CHANNEL,), required=True)
    package.add_argument("--build-id", required=True)
    package.add_argument("--released-at", required=True)
    package.add_argument("--source-sha", required=True)
    package.add_argument("--cargo-lock", required=True)
    package.add_argument("--vst3-sdk-revision", default=VST3_SDK_REVISION)
    package.add_argument("--runner-image", required=True)
    package.add_argument("--runner-image-os", required=True)
    package.add_argument("--runner-image-version", required=True)
    package.add_argument("--rust-toolchain", required=True)
    package.add_argument("--rust-target", required=True)
    package.add_argument("--rustc-version", required=True)
    package.add_argument("--python-implementation", required=True)
    package.add_argument("--python-version", required=True)
    package.set_defaults(handler=_package_command)
    validate = subparsers.add_parser("validate", help="validate an emitted Windows VST3 manifest and archive")
    validate.add_argument("--root", required=True)
    validate.add_argument("--cargo-lock", required=True)
    validate.add_argument("--vst3-sdk-revision", default=VST3_SDK_REVISION)
    validate.set_defaults(handler=_validate_command)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    try:
        args.handler(args)
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        print(f"windows release packaging failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
