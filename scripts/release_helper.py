#!/usr/bin/env python3
"""Small, dependency-free helpers for the WAVE release producer."""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import platform
import struct
import re
import subprocess
import tempfile
from pathlib import Path
from typing import Any, Callable, Optional, Sequence
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

MANIFEST_SCHEMA = 2
MANIFEST_CONTENT_TYPE = "application/vnd.portalsurfer.release-manifest+json;version=2"
PRODUCTION_ORIGIN = "https://portalsurfer.org"
SAFE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
TEAM_ID = re.compile(r"[A-Z0-9]{10}\Z")
NOTARY_ID = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-5][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}\Z")
SEMVER = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?\Z")
CORE_SEMVER = re.compile(r"([0-9]+)\.([0-9]+)\.([0-9]+)\Z")
GIT_SHA = re.compile(r"[0-9a-f]{40}\Z")


def latest_release_source_sha(document: Any, *, channel: str) -> Optional[str]:
    """Return the newest validated source SHA for ``channel``.

    The releases endpoint is an external release decision input, so malformed
    matching records fail closed instead of being silently ignored.  Selection
    uses parsed timezone-aware RFC3339 timestamps rather than response order.
    """
    if channel not in {"stable", "rc", "nightly"}:
        raise ValueError(f"invalid release channel: {channel}")
    if not isinstance(document, dict) or not isinstance(document.get("releases"), list):
        raise ValueError("release history must contain a releases array")

    latest: tuple[dt.datetime, str] | None = None
    for release in document["releases"]:
        if not isinstance(release, dict):
            raise ValueError("release history contains a non-object release")
        release_channel = release.get("channel")
        if not isinstance(release_channel, str):
            raise ValueError("release history contains a release without a channel")
        if release_channel != channel:
            continue
        released_at = release.get("released_at")
        if not isinstance(released_at, str):
            raise ValueError("matching release is missing released_at")
        try:
            parsed = dt.datetime.fromisoformat(released_at.replace("Z", "+00:00"))
        except ValueError as error:
            raise ValueError("matching release released_at must be RFC3339") from error
        if parsed.tzinfo is None or parsed.utcoffset() is None:
            raise ValueError("matching release released_at must include a timezone")
        source = release.get("source")
        if not isinstance(source, dict) or source.get("repository") != "PORTALSURFER/wave":
            raise ValueError("matching release source repository is invalid")
        source_sha = source.get("git_sha")
        if not isinstance(source_sha, str) or not GIT_SHA.fullmatch(source_sha):
            raise ValueError("matching release source git_sha is invalid")
        candidate = (parsed, source_sha)
        if latest is None or candidate[0] > latest[0]:
            latest = candidate
    return latest[1] if latest is not None else None


def should_release(*, source_sha: str, document: Any = None, channel: str = "nightly", only_if_changed: bool = True) -> bool:
    """Decide whether a release workflow should perform production work.

    The bypass path intentionally does not inspect ``document``; ordinary
    stable/RC/manual releases therefore preserve their existing behavior and
    make zero public API requests.
    """
    if not isinstance(source_sha, str) or not GIT_SHA.fullmatch(source_sha):
        raise ValueError("checked-out source SHA is invalid")
    if not only_if_changed:
        return True
    latest = latest_release_source_sha(document, channel=channel)
    return latest is None or latest != source_sha


def _parse_core_version(version: Any, *, field: str) -> tuple[int, int, int]:
    if not isinstance(version, str):
        raise ValueError(f"{field} must be a numeric semver")
    match = CORE_SEMVER.fullmatch(version)
    if match is None:
        raise ValueError(f"{field} must be a numeric semver")
    return tuple(int(part) for part in match.groups())


def _format_core_version(version: tuple[int, int, int]) -> str:
    return ".".join(str(part) for part in version)


def latest_release_version(document: Any) -> Optional[str]:
    """Return the highest numeric version published on any release channel."""
    if not isinstance(document, dict) or not isinstance(document.get("releases"), list):
        raise ValueError("release history must contain a releases array")

    latest: tuple[int, int, int] | None = None
    for release in document["releases"]:
        if not isinstance(release, dict):
            raise ValueError("release history contains a non-object release")
        if release.get("channel") not in {"stable", "rc", "nightly"}:
            raise ValueError("release history contains an invalid channel")
        candidate = _parse_core_version(release.get("version"), field="release version")
        if latest is None or candidate > latest:
            latest = candidate
    return _format_core_version(latest) if latest is not None else None


def next_release_version(package_version: str, document: Any) -> str:
    """Select one globally increasing patch version for the next release.

    A package version already ahead of public history is treated as a pending
    bump from a previously interrupted release.  Reusing it avoids consuming
    another patch number on retry; otherwise every successful release advances
    the highest published version by exactly one patch.
    """
    current = _parse_core_version(package_version, field="package version")
    latest_text = latest_release_version(document)
    latest = _parse_core_version(latest_text, field="latest release version") if latest_text else None
    if latest is not None and current > latest:
        return _format_core_version(current)
    base = latest if latest is not None and latest > current else current
    return _format_core_version((base[0], base[1], base[2] + 1))


def validate_release_fields(version: str, released_at: str, names: list[str], hashes: list[str], sizes: list[int]) -> None:
    if not SEMVER.fullmatch(version):
        raise ValueError("version must be semver-like")
    try:
        parsed = dt.datetime.fromisoformat(released_at.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError("released_at must be RFC3339") from error
    if parsed.tzinfo is None:
        raise ValueError("released_at must include a timezone")
    if len(names) != len(set(names)) or any(not SAFE_NAME.fullmatch(name) for name in names):
        raise ValueError("release file names must be unique safe basenames")
    if any(not SHA256.fullmatch(value) for value in hashes):
        raise ValueError("release hashes must be lowercase SHA-256")
    if any(not isinstance(size, int) or size <= 0 for size in sizes):
        raise ValueError("release sizes must be positive integers")


def canonical_json(value: Any) -> bytes:
    """Encode JSON deterministically; this byte representation is the commit body."""
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def file_digest(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def validate_png(path: Path, width: int = 960, height: int = 600) -> dict[str, Any]:
    """Validate the structural PNG contract used by the default UI capture."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    offset = 8
    seen_ihdr = False
    seen_iend = False
    seen_idat = False
    dimensions: Optional[tuple[int, int]] = None
    color_type: int | None = None
    dpi = 1.0
    while offset + 12 <= len(data):
        length = struct.unpack(">I", data[offset : offset + 4])[0]
        kind = data[offset + 4 : offset + 8]
        end = offset + 12 + length
        if end > len(data):
            raise ValueError(f"{path} has a truncated PNG chunk")
        payload = data[offset + 8 : offset + 8 + length]
        crc = struct.unpack(">I", data[offset + 8 + length : end])[0]
        if crc != (zlib_crc32(kind + payload)):
            raise ValueError(f"{path} has an invalid {kind.decode('ascii', 'replace')} CRC")
        if kind == b"IHDR":
            if length != 13 or seen_ihdr:
                raise ValueError(f"{path} has an invalid IHDR")
            seen_ihdr = True
            dimensions = struct.unpack(">II", payload[:8])
            bit_depth, color_type = payload[8], payload[9]
            if bit_depth != 8 or color_type not in (2, 6):
                raise ValueError(f"{path} must use 8-bit RGB or RGBA pixels")
        elif kind == b"pHYs" and length == 9:
            x, y, unit = struct.unpack(">IIB", payload)
            if unit == 1:
                if x == 0 or y == 0 or x != y:
                    raise ValueError(f"{path} has a non-1.0 pixel scale")
                dpi = 1.0
        elif kind == b"IDAT":
            seen_idat = True
        elif kind == b"IEND":
            if length != 0:
                raise ValueError(f"{path} has an invalid IEND")
            seen_iend = True
            if end != len(data):
                raise ValueError(f"{path} has bytes after IEND")
            break
        offset = end
    if not seen_ihdr or not seen_idat or not seen_iend or dimensions != (width, height):
        missing = []
        if not seen_ihdr: missing.append("IHDR")
        if not seen_idat: missing.append("IDAT")
        if not seen_iend: missing.append("IEND")
        raise ValueError(f"{path} must be {width}x{height} with IHDR, IDAT, and IEND (missing {', '.join(missing) or 'valid dimensions'})")
    digest, size = file_digest(path)
    return {"width": width, "height": height, "dpi": dpi, "hash": digest, "size": size}


def zlib_crc32(data: bytes) -> int:
    import zlib

    return zlib.crc32(data) & 0xFFFFFFFF


def build_manifest(
    *,
    version: str,
    build_id: str,
    channel: str,
    released_at: str,
    git_sha: str,
    clap: Path,
    vst3: Path,
    screenshot: Path,
    changelog: Path,
    distribution: str = "production",
    signing_identity_class: str = "Developer ID Application",
    notarized: bool = True,
    stapled: bool = True,
    signing_team_id: str = "",
    notary_submissions: Optional[dict[str, str]] = None,
) -> dict[str, Any]:
    if channel not in {"stable", "rc", "nightly"}:
        raise ValueError(f"invalid channel: {channel}")
    if distribution not in {"production", "preflight"}:
        raise ValueError(f"invalid distribution: {distribution}")
    if distribution == "production" and (signing_identity_class != "Developer ID Application" or not notarized or not stapled or not isinstance(signing_team_id, str) or not TEAM_ID.fullmatch(signing_team_id) or set(notary_submissions or {}) != {"clap", "vst3"} or any(not isinstance(notary_id, str) or not NOTARY_ID.fullmatch(notary_id) for notary_id in (notary_submissions or {}).values())):
        raise ValueError("production manifests require Developer ID signing and a stapled notarization")
    if distribution == "preflight" and (signing_identity_class != "ad hoc" or notarized or stapled or signing_team_id or notary_submissions):
        raise ValueError("preflight manifests require ad hoc, non-notarized provenance")
    screenshot_info = validate_png(screenshot)
    artifacts = []
    for fmt, path in (("clap", clap), ("vst3", vst3)):
        digest, size = file_digest(path)
        artifacts.append(
            {
                "format": fmt,
                "platform": "macos",
                "architectures": ["arm64"],
                "name": path.name,
                "media_type": "application/zip",
                "sha256": digest,
                "size_bytes": size,
            }
        )
    changelog_hash, changelog_size = file_digest(changelog)
    if changelog_size == 0:
        raise ValueError("CHANGELOG.md must not be empty")
    validate_release_fields(version, released_at, [item["name"] for item in artifacts] + [screenshot.name, changelog.name], [item["sha256"] for item in artifacts] + [screenshot_info["hash"], changelog_hash], [item["size_bytes"] for item in artifacts] + [screenshot_info["size"], changelog_size])
    return {
        "schema_version": MANIFEST_SCHEMA,
        "product": "wave",
        "build_id": build_id,
        "version": version,
        "channel": channel,
        "released_at": released_at,
        "source": {"repository": "PORTALSURFER/wave", "git_sha": git_sha, "dirty": False},
        "distribution": distribution,
        "signing": {
            "identity_class": signing_identity_class,
            "notarized": notarized,
            "stapled": stapled,
            "team_id": signing_team_id,
            "notary_submissions": notary_submissions or {},
        },
        "artifacts": artifacts,
        "screenshot": {
            "role": "default-ui",
            "name": screenshot.name,
            "media_type": "image/png",
            "width": screenshot_info["width"],
            "height": screenshot_info["height"],
            "logical_width": screenshot_info["width"],
            "logical_height": screenshot_info["height"],
            "dpi_scale": screenshot_info["dpi"],
            "source_git_sha": git_sha,
            "sha256": screenshot_info["hash"],
            "size_bytes": screenshot_info["size"],
        },
        "changelog": {
            "name": changelog.name,
            "format": "markdown",
            "media_type": "text/markdown; charset=utf-8",
            "sha256": changelog_hash,
            "size_bytes": changelog_size,
        },
    }


def _request(url: str, method: str, body: Optional[bytes], headers: dict[str, str]) -> tuple[int, bytes]:
    request = Request(url, method=method, data=body, headers=headers)
    try:
        with urlopen(request) as response:
            return response.status, response.read()
    except (HTTPError, URLError) as error:
        if isinstance(error, HTTPError):
            detail = error.read().decode("utf-8", "replace")
            raise RuntimeError(f"{method} {url} failed ({error.code}): {detail[:400]}") from error
        raise RuntimeError(f"{method} {url} failed: {error.reason}") from error


Transport = Callable[[str, str, Optional[bytes], dict[str, str]], tuple[int, bytes]]


def _publish_validated_manifest(
    *,
    endpoint: str,
    token: str,
    manifest: dict[str, Any],
    root: Path,
    transport: Transport,
) -> None:
    """Publish a manifest after the caller has completed all local validation."""
    request = transport
    base = endpoint
    status, payload = request(f"{base}/plugins/api/v1/products/wave/releases", "GET", None, {"Accept": "application/json"})
    if status < 200 or status >= 300:
        raise RuntimeError(f"capability check failed ({status})")
    try:
        capability = json.loads(payload)
    except json.JSONDecodeError as error:
        raise RuntimeError("capability response was not JSON") from error
    versions = capability.get("release_upload", {}).get("manifest_schema_versions", [])
    if MANIFEST_SCHEMA not in versions:
        raise RuntimeError("server does not support release manifest schema 2; no files were uploaded")

    files: list[tuple[str, Path, str]] = []
    for artifact in manifest["artifacts"]:
        files.append((artifact["name"], root / artifact["name"], artifact["sha256"]))
    screenshot = manifest["screenshot"]
    files.append((screenshot["name"], root / screenshot["name"], screenshot["sha256"]))
    changelog = manifest["changelog"]
    files.append((changelog["name"], root / changelog["name"], changelog["sha256"]))
    metadata = {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/octet-stream",
        "X-PortalSurfer-Release-Version": manifest["version"],
        "X-PortalSurfer-Release-Channel": manifest["channel"],
        "X-PortalSurfer-Released-At": manifest["released_at"],
    }
    for name, path, digest in files:
        data = path.read_bytes()
        headers = {**metadata, "Content-Length": str(len(data)), "X-PortalSurfer-Sha256": digest}
        request(
            f"{base}/plugins/api/v1/products/wave/release-uploads/{manifest['build_id']}/staging/files/{name}",
            "PUT",
            data,
            headers,
        )

    body = canonical_json(manifest)
    manifest_hash = hashlib.sha256(body).hexdigest()
    commit_headers = {
        "Authorization": f"Bearer {token}",
        "Content-Type": MANIFEST_CONTENT_TYPE,
        "Content-Length": str(len(body)),
        "X-PortalSurfer-Manifest-Sha256": manifest_hash,
        "X-PortalSurfer-Release-Version": manifest["version"],
        "X-PortalSurfer-Release-Channel": manifest["channel"],
        "X-PortalSurfer-Released-At": manifest["released_at"],
    }
    request(
        f"{base}/plugins/api/v1/products/wave/release-uploads/{manifest['build_id']}/commit",
        "PUT",
        body,
        commit_headers,
    )


def _run_checked(args: Sequence[str], *, cwd: Path, capture_output: bool = False) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            list(args),
            cwd=str(cwd),
            check=True,
            capture_output=capture_output,
            text=True,
        )
    except FileNotFoundError as error:
        raise RuntimeError(f"required release command is unavailable: {args[0]}") from error
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or "").strip()
        raise ValueError(f"release command failed: {' '.join(args)}{': ' + detail if detail else ''}") from error


def _repo_output(args: Sequence[str], repo_root: Path) -> str:
    return _run_checked(args, cwd=repo_root, capture_output=True).stdout.strip()


def _validate_canonical_source(manifest: dict[str, Any], repo_root: Path) -> None:
    try:
        branch = _repo_output(("git", "symbolic-ref", "--quiet", "--short", "HEAD"), repo_root)
    except ValueError:
        branch = ""
    if not branch:
        raise ValueError("production publishing requires a non-detached checkout")
    dirty_status = _run_checked(
        ("git", "status", "--porcelain", "--untracked-files=all"),
        cwd=repo_root,
        capture_output=True,
    ).stdout.rstrip("\r\n")
    if dirty_status:
        entries = " | ".join(dirty_status.splitlines())
        raise ValueError(f"production release source must be clean; git status entries: {entries}")
    _run_checked(("git", "fetch", "origin", "main", "--quiet"), cwd=repo_root)
    head = _repo_output(("git", "rev-parse", "HEAD"), repo_root)
    canonical_main = _repo_output(("git", "rev-parse", "refs/remotes/origin/main"), repo_root)
    source_sha = manifest["source"]["git_sha"]
    if head != source_sha or canonical_main != source_sha:
        raise ValueError("production release source must match HEAD, origin/main, and manifest source SHA")


def _assert_no_symlinks(path: Path) -> None:
    if path.is_symlink():
        raise ValueError(f"release ZIP contains an unexpected symlink: {path.name}")


def _audit_zip(path: Path, format_name: str, expected_team: str, *, cwd: Path, require_production: bool = True) -> None:
    if platform.system() != "Darwin":
        raise ValueError("production ZIP audits require macOS")
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"release ZIP is not a regular file: {path.name}")
    with tempfile.TemporaryDirectory(prefix="wave-release-audit-") as temporary:
        extracted = Path(temporary)
        _run_checked(("/usr/bin/ditto", "-x", "-k", str(path), str(extracted)), cwd=cwd)
        bundle = extracted / f"wave.{format_name}"
        contents = bundle / "Contents"
        required = {
            bundle,
            contents,
            contents / "Info.plist",
            contents / "PkgInfo",
            contents / "MacOS",
            contents / "MacOS" / "wave",
        }
        allowed = required | {contents / "CodeResources", contents / "_CodeSignature", contents / "_CodeSignature" / "CodeResources"}
        code_resources = contents / "CodeResources"
        if code_resources.exists() and (code_resources.is_symlink() or not code_resources.is_file()):
            raise ValueError(f"{format_name} ZIP Contents/CodeResources must be a regular file")
        if not bundle.is_dir() or not contents.is_dir():
            raise ValueError(f"{format_name} ZIP bundle layout is invalid")
        for current, directories, files in os.walk(extracted, followlinks=False):
            current_path = Path(current)
            _assert_no_symlinks(current_path)
            for name in directories + files:
                child = current_path / name
                _assert_no_symlinks(child)
                if child not in allowed:
                    raise ValueError(f"{format_name} ZIP contains unexpected topology: {child.relative_to(extracted)}")
        if not all(p.exists() for p in required) or not (contents / "MacOS" / "wave").is_file() or not os.access(contents / "MacOS" / "wave", os.X_OK):
            raise ValueError(f"{format_name} ZIP bundle layout is invalid")
        info = contents / "Info.plist"
        binary = contents / "MacOS" / "wave"
        _run_checked(("/usr/bin/plutil", "-lint", str(info)), cwd=cwd)
        identifier = _run_checked(("/usr/bin/plutil", "-extract", "CFBundleIdentifier", "raw", "-o", "-", str(info)), cwd=cwd, capture_output=True).stdout.strip()
        if identifier != f"com.portalsurfer.wave.{format_name}":
            raise ValueError(f"{format_name} ZIP bundle identifier is invalid")
        package_type = _run_checked(("/usr/bin/plutil", "-extract", "CFBundlePackageType", "raw", "-o", "-", str(info)), cwd=cwd, capture_output=True).stdout.strip()
        if package_type != "BNDL":
            raise ValueError(f"{format_name} ZIP package type is invalid")
        _run_checked(("codesign", "--verify", "--deep", "--strict", str(bundle)), cwd=cwd)
        if require_production:
            details = _run_checked(("codesign", "-dv", "--verbose=4", str(bundle)), cwd=cwd, capture_output=True)
            signing_details = f"{details.stdout}\n{details.stderr}"
            if not any(line.startswith("Authority=Developer ID Application:") for line in signing_details.splitlines()):
                raise ValueError(f"{format_name} ZIP is not signed by a Developer ID Application authority")
            team_ids = [line.removeprefix("TeamIdentifier=") for line in signing_details.splitlines() if line.startswith("TeamIdentifier=")]
            if team_ids != [expected_team]:
                raise ValueError(f"{format_name} ZIP Developer ID signing team does not match manifest")
            _run_checked(("xcrun", "stapler", "validate", str(bundle)), cwd=cwd)
            _run_checked(("codesign", "-vvvv", "-R=notarized", "--check-notarization", str(bundle)), cwd=cwd)
        architectures = _run_checked(("lipo", "-archs", str(binary)), cwd=cwd, capture_output=True).stdout.strip()
        if architectures != "arm64":
            raise ValueError(f"{format_name} ZIP binary must contain exactly arm64")
        symbols = _run_checked(("/usr/bin/nm", "-gU", str(binary)), cwd=cwd, capture_output=True).stdout
        required_symbols = ("_clap_entry",) if format_name == "clap" else ("_GetPluginFactory", "_bundleEntry", "_bundleExit")
        if any(symbol not in symbols for symbol in required_symbols):
            raise ValueError(f"{format_name} ZIP required export is missing")


def _validate_exact_manifest_names(manifest: dict[str, Any]) -> None:
    expected = {"clap": f"wave-v{manifest['version']}-macos.clap.zip", "vst3": f"wave-v{manifest['version']}-macos.vst3.zip"}
    actual = {artifact["format"]: artifact["name"] for artifact in manifest["artifacts"]}
    if actual != expected:
        raise ValueError("manifest artifact names do not match the exact WAVE ZIP contract")
    if manifest["screenshot"]["role"] != "default-ui" or manifest["screenshot"]["name"] != "wave-default-960x600.png" or manifest["changelog"]["name"] != "CHANGELOG.md":
        raise ValueError("manifest support-file roles or names do not match the WAVE release contract")


def publish_release(*, endpoint: str, token: str, manifest_path: Path, root: Optional[Path] = None, repo_root: Optional[Path] = None) -> None:
    """Validate and publish one production WAVE manifest through the real request path."""
    if endpoint != PRODUCTION_ORIGIN:
        raise ValueError(f"production publishing requires exact origin {PRODUCTION_ORIGIN}")
    if not token:
        raise ValueError("PORTALSURFER_RELEASE_TOKEN is required for --publish")
    manifest_path = Path(manifest_path)
    artifact_root = Path(root) if root is not None else manifest_path.parent
    source_root = Path(repo_root) if repo_root is not None else Path.cwd()
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("release manifest must be a regular file")
    try:
        manifest_bytes = manifest_path.read_bytes()
        manifest = json.loads(manifest_bytes)
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"could not load release manifest: {manifest_path}") from error
    if not isinstance(manifest, dict) or canonical_json(manifest) != manifest_bytes:
        raise ValueError("release manifest is not canonical JSON")
    validate_publish_manifest(manifest, artifact_root)
    _validate_exact_manifest_names(manifest)
    _validate_canonical_source(manifest, source_root)
    expected_team = manifest["signing"]["team_id"]
    for artifact in manifest["artifacts"]:
        _audit_zip(artifact_root / artifact["name"], artifact["format"], expected_team, cwd=source_root)
    for entry in [*manifest["artifacts"], manifest["screenshot"], manifest["changelog"]]:
        path = artifact_root / entry["name"]
        if path.is_symlink():
            raise ValueError(f"release file must not be a symlink: {entry['name']}")
        digest, size = file_digest(path)
        if digest != entry["sha256"] or size != entry["size_bytes"]:
            raise ValueError(f"release bytes changed after ZIP audit: {entry['name']}")
    _publish_validated_manifest(endpoint=endpoint, token=token, manifest=manifest, root=artifact_root, transport=_request)


def validate_publish_manifest(manifest: dict[str, Any], root: Path, *, require_production: bool = True) -> None:
    if manifest.get("schema_version") != MANIFEST_SCHEMA or manifest.get("product") != "wave":
        raise ValueError("publish requires WAVE manifest schema 2")
    build_id = manifest.get("build_id")
    source = manifest.get("source", {})
    signing = manifest.get("signing", {})
    if not isinstance(signing, dict):
        raise ValueError("publish manifest has invalid signing provenance")
    if not isinstance(build_id, str) or not re.fullmatch(r"[a-z0-9][a-z0-9._-]{1,127}", build_id) or not isinstance(source, dict) or source != {"repository": "PORTALSURFER/wave", "git_sha": source.get("git_sha"), "dirty": False} or not isinstance(source.get("git_sha"), str) or not re.fullmatch(r"[0-9a-f]{40}", source["git_sha"]):
        raise ValueError("publish manifest has invalid immutable source")
    if require_production:
        if manifest.get("distribution") != "production" or signing.get("identity_class") != "Developer ID Application" or signing.get("notarized") is not True or signing.get("stapled") is not True:
            raise ValueError("publish requires production Developer ID notarized provenance")
        if not TEAM_ID.fullmatch(signing.get("team_id", "")):
            raise ValueError("publish requires a valid Developer Team ID")
    elif manifest.get("distribution") != "preflight" or signing.get("identity_class") != "ad hoc" or signing.get("notarized") is not False or signing.get("stapled") is not False or signing.get("team_id") != "":
        raise ValueError("preflight requires ad hoc non-notarized provenance")
    submissions = signing.get("notary_submissions", {})
    if require_production:
        if set(submissions) != {"clap", "vst3"} or any(not isinstance(value, str) or not NOTARY_ID.fullmatch(value) for value in submissions.values()):
            raise ValueError("publish requires accepted notarization submission ids")
    elif submissions:
        raise ValueError("preflight manifests cannot contain notarization submission ids")
    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list) or len(artifacts) != 2 or not all(isinstance(a, dict) for a in artifacts) or {a.get("format") for a in artifacts} != {"clap", "vst3"}:
        raise ValueError("publish requires exactly CLAP and VST3 artifacts")
    screenshot = manifest.get("screenshot")
    changelog = manifest.get("changelog")
    entries = list(artifacts) + [screenshot, changelog]
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("name"), str) or not isinstance(entry.get("sha256"), str) or not isinstance(entry.get("size_bytes"), int):
            raise ValueError("publish manifest has invalid file metadata")
    validate_release_fields(manifest.get("version", ""), manifest.get("released_at", ""), [entry["name"] for entry in entries], [entry["sha256"] for entry in entries], [entry["size_bytes"] for entry in entries])
    for entry in entries:
        path = root / entry["name"]
        digest, size = file_digest(path)
        if digest != entry["sha256"] or size != entry["size_bytes"]:
            raise ValueError(f"on-disk bytes do not match manifest: {entry['name']}")
    for artifact in artifacts:
        if artifact.get("platform") != "macos" or artifact.get("architectures") != ["arm64"] or artifact.get("media_type") != "application/zip":
            raise ValueError("publish artifacts must be macOS arm64")
    screenshot = manifest["screenshot"]
    if (screenshot.get("role") != "default-ui" or screenshot.get("media_type") != "image/png" or
            screenshot.get("width") != 960 or screenshot.get("height") != 600 or
            screenshot.get("logical_width") != 960 or screenshot.get("logical_height") != 600 or
            screenshot.get("dpi_scale") != 1.0 or screenshot.get("source_git_sha") != source["git_sha"]):
        raise ValueError("publish screenshot metadata does not match the WAVE UI contract")
    validate_png(root / screenshot["name"])
    changelog = manifest["changelog"]
    if changelog.get("format") != "markdown" or changelog.get("media_type") != "text/markdown; charset=utf-8":
        raise ValueError("publish changelog metadata does not match the release contract")


def validate_preflight_manifest(manifest: dict[str, Any], root: Path) -> None:
    """Validate local preflight provenance and the same file/metadata contract as publish."""
    validate_publish_manifest(manifest, root, require_production=False)
    _validate_exact_manifest_names(manifest)
