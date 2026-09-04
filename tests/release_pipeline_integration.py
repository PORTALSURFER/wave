#!/usr/bin/env python3
"""Validate the WAVE nightly artifact and publisher-integration contracts."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
from typing import Any, Iterator
from urllib.parse import parse_qs, quote, unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "scripts"
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

import release_helper
import windows_release_helper


PUBLISHER_COMMIT = "165776d6707ab6d9e8bb76b2a8866654140ca6bc"
TEST_RELEASE_TOKEN = "wave-integration-release-token"
TEST_OIDC_REQUEST_TOKEN = "wave-integration-oidc-request-token"
TEST_ATTESTATION_TOKEN = "wave-integration-attestation-token"
TEST_NOTARY_ID = "00000000-0000-4000-8000-000000000000"
CAPABILITY_PATH = "/plugins/api/v1/products/wave/releases"
MANIFEST_CONTENT_TYPE = "application/vnd.portalsurfer.release-manifest+json;version=3"


class HarnessError(RuntimeError):
    """A concise, non-secret integration failure."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise HarnessError(message)


def _redact(value: str) -> str:
    for secret in (TEST_RELEASE_TOKEN, TEST_OIDC_REQUEST_TOKEN, TEST_ATTESTATION_TOKEN):
        value = value.replace(secret, "<redacted>")
    return value


def _regular_file(path: Path, label: str) -> None:
    require(not path.is_symlink() and path.is_file(), f"{label} is not a regular file: {path.name}")


def _find_unique(root: Path, name: str, label: str) -> Path:
    require(not root.is_symlink() and root.is_dir(), f"{label} root is not a directory")
    matches = sorted(root.rglob(name))
    require(len(matches) == 1, f"{label} must contain exactly one {name}")
    _regular_file(matches[0], label)
    return matches[0]


def _load_json(path: Path, label: str) -> dict[str, Any]:
    _regular_file(path, label)
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise HarnessError(f"could not read {label}: {error}") from error
    require(isinstance(value, dict), f"{label} must be a JSON object")
    return value


def _exact_entries(root: Path, names: set[str], label: str) -> None:
    entries = list(root.iterdir())
    require(
        len(entries) == len(names)
        and {entry.name for entry in entries} == names
        and all(not entry.is_symlink() and entry.is_file() for entry in entries),
        f"{label} contains unexpected files",
    )


def _shared_source(manifest: dict[str, Any], source_sha: str, label: str) -> None:
    source = manifest.get("source")
    require(isinstance(source, dict), f"{label} source metadata is invalid")
    require(source.get("repository") == "PORTALSURFER/wave", f"{label} repository is invalid")
    require(source.get("git_sha") == source_sha, f"{label} source SHA does not match prepare")
    require(source.get("dirty") is False, f"{label} source is not clean")


def _load_macos_preflight(
    artifact_root: Path,
    *,
    package_version: str,
    build_id: str,
    source_sha: str,
    released_at: str,
) -> tuple[dict[str, Any], Path, Path, Path, Path, Path]:
    manifest_path = _find_unique(artifact_root, "release-manifest.json", "macOS preflight artifact")
    root = manifest_path.parent
    manifest = _load_json(manifest_path, "macOS preflight manifest")
    try:
        release_helper.validate_preflight_manifest(manifest, root, package_version=package_version)
    except (OSError, ValueError) as error:
        raise HarnessError(f"macOS preflight manifest validation failed: {error}") from error
    require(manifest_path.read_bytes() == release_helper.canonical_json(manifest), "macOS preflight manifest is not canonical JSON")

    require(manifest.get("schema_version") == release_helper.MANIFEST_SCHEMA_V2, "macOS preflight must use schema 2")
    require(manifest.get("product") == "wave", "macOS preflight product is invalid")
    require(manifest.get("distribution") == "preflight", "macOS artifact must be a preflight distribution")
    require(manifest.get("channel") == "stable", "macOS preflight must use the stable channel")
    require(manifest.get("version") == package_version, "macOS preflight version does not match prepare")
    require(manifest.get("build_id") == build_id, "macOS preflight build id does not match prepare")
    require(manifest.get("released_at") == released_at, "macOS preflight timestamp does not match prepare")
    _shared_source(manifest, source_sha, "macOS preflight")

    clap_name = f"wave-v{package_version}-macos.clap.zip"
    vst3_name = f"wave-v{package_version}-macos.vst3.zip"
    artifacts = manifest.get("artifacts")
    require(
        isinstance(artifacts, list)
        and [(item.get("format"), item.get("name")) for item in artifacts]
        == [("clap", clap_name), ("vst3", vst3_name)],
        "macOS preflight artifacts do not match the WAVE schema-2 contract",
    )
    screenshot = manifest.get("screenshot")
    changelog = manifest.get("changelog")
    require(isinstance(screenshot, dict), "macOS preflight screenshot metadata is invalid")
    require(screenshot.get("name") == "wave-default-960x600.png", "macOS preflight screenshot name is invalid")
    require(isinstance(changelog, dict) and changelog.get("name") == "CHANGELOG.md", "macOS preflight changelog is invalid")
    _exact_entries(
        root,
        {"release-manifest.json", clap_name, vst3_name, screenshot["name"], changelog["name"]},
        "macOS preflight artifact",
    )
    return (
        manifest,
        root,
        root / clap_name,
        root / vst3_name,
        root / screenshot["name"],
        root / changelog["name"],
    )


def _load_windows_artifact(
    artifact_root: Path,
    *,
    package_version: str,
    publication_version: str,
    build_id: str,
    source_sha: str,
    released_at: str,
) -> tuple[dict[str, Any], Path, Path]:
    manifest_path = _find_unique(artifact_root, "windows-artifact-manifest.json", "Windows artifact")
    root = manifest_path.parent
    manifest = _load_json(manifest_path, "Windows artifact manifest")
    try:
        windows_release_helper.validate_manifest(
            manifest,
            root,
            cargo_lock=ROOT / "Cargo.lock",
            vst3_sdk_revision=windows_release_helper.VST3_SDK_REVISION,
        )
    except (OSError, ValueError) as error:
        raise HarnessError(f"Windows artifact validation failed: {error}") from error

    source = manifest.get("source")
    archive = manifest.get("archive")
    archive_name = f"wave-v{publication_version}-windows-x86_64-unsigned.vst3.zip"
    require(manifest.get("schema_version") == 1, "Windows artifact schema is invalid")
    require(manifest.get("product") == "wave", "Windows artifact product is invalid")
    require(manifest.get("format") == "vst3", "Windows artifact format is invalid")
    require(manifest.get("platform") == "windows" and manifest.get("architecture") == "x86_64", "Windows artifact target is invalid")
    require(manifest.get("package_version") == package_version, "Windows package version does not match prepare")
    require(manifest.get("publication_version") == publication_version, "Windows publication version does not match prepare")
    require(manifest.get("channel") == "nightly", "Windows artifact must use the nightly channel")
    require(manifest.get("build_id") == build_id, "Windows build id does not match prepare")
    require(manifest.get("released_at") == released_at, "Windows timestamp does not match prepare")
    _shared_source(manifest, source_sha, "Windows artifact")
    require(manifest.get("signing_status") == "unsigned" and manifest.get("signing_certificate") is None, "Windows artifact must be unsigned")
    require(isinstance(archive, dict) and archive.get("name") == archive_name, "Windows archive name does not match prepare")
    _exact_entries(root, {"windows-artifact-manifest.json", archive_name}, "Windows artifact")
    return manifest, root, root / archive_name


def _copy(source: Path, destination: Path, label: str) -> None:
    _regular_file(source, label)
    require(not destination.exists(), f"refusing to overwrite {destination}")
    shutil.copyfile(source, destination)


def _manifest_files(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    values = [*manifest["artifacts"], manifest["screenshot"], manifest["changelog"]]
    require(all(isinstance(value, dict) for value in values), "schema 3 file metadata is invalid")
    return values


class Trace:
    def __init__(self) -> None:
        self.events: list[str] = []
        self.unexpected: list[str] = []

    def reset(self) -> None:
        self.events.clear()
        self.unexpected.clear()


def _send_response(handler: http.server.BaseHTTPRequestHandler, status: int, body: bytes, content_type: str) -> None:
    handler.send_response(status)
    handler.send_header("Content-Type", content_type)
    handler.send_header("Content-Length", str(len(body)))
    handler.send_header("Connection", "close")
    handler.end_headers()
    if body and handler.command != "HEAD":
        handler.wfile.write(body)
    handler.close_connection = True


def _send_json(handler: http.server.BaseHTTPRequestHandler, status: int, value: dict[str, Any]) -> None:
    _send_response(handler, status, json.dumps(value, separators=(",", ":")).encode("utf-8"), "application/json")


def _reject(trace: Trace, handler: http.server.BaseHTTPRequestHandler, label: str) -> None:
    trace.unexpected.append(label)
    _send_response(handler, 400, b"unexpected request\n", "text/plain; charset=utf-8")


def _request_body(handler: http.server.BaseHTTPRequestHandler) -> bytes | None:
    raw_length = handler.headers.get("Content-Length")
    if raw_length is None:
        return b""
    try:
        length = int(raw_length)
    except ValueError:
        return None
    if length < 0:
        return None
    body = handler.rfile.read(length)
    return body if len(body) == length else None


def _url_parts(handler: http.server.BaseHTTPRequestHandler) -> tuple[str, str] | None:
    parsed = urlsplit(handler.path)
    if parsed.scheme or parsed.netloc or parsed.fragment:
        return None
    return parsed.path, parsed.query


def _header_is(handler: http.server.BaseHTTPRequestHandler, name: str, expected: str) -> bool:
    return handler.headers.get(name) == expected


def _header_absent(handler: http.server.BaseHTTPRequestHandler, name: str) -> bool:
    return handler.headers.get(name) is None


class ApiMock:
    def __init__(self, manifest: dict[str, Any], root: Path, trace: Trace) -> None:
        self.manifest = manifest
        self.root = root
        self.trace = trace
        self.phase = "missing_oidc"
        self.stage_names = [file["name"] for file in _manifest_files(manifest)]
        self.stage_records: list[str] = []
        self.commit_count = 0

    def reset_for_success(self) -> None:
        self.phase = "success"
        self.stage_records.clear()
        self.commit_count = 0
        self.trace.reset()

    def _release_upload_prefix(self) -> str:
        build_id = quote(self.manifest["build_id"], safe="")
        return f"/plugins/api/v1/products/wave/release-uploads/{build_id}"

    def _capability(self, handler: http.server.BaseHTTPRequestHandler, method: str, path: str, query: str, body: bytes | None) -> None:
        if (
            method != "GET"
            or path != CAPABILITY_PATH
            or query
            or body != b""
            or self.trace.events
            or not _header_is(handler, "Accept", "application/json")
            or not _header_absent(handler, "Authorization")
        ):
            _reject(self.trace, handler, "unexpected capability request")
            return
        self.trace.events.append("capability")
        _send_json(
            handler,
            200,
            {
                "release_upload": {
                    "manifest_schema_versions": [3],
                    "manifest_content_types": {"3": MANIFEST_CONTENT_TYPE},
                    "artifact_counts": {"3": 3},
                }
            },
        )

    def _stage(
        self,
        handler: http.server.BaseHTTPRequestHandler,
        method: str,
        path: str,
        query: str,
        body: bytes | None,
    ) -> None:
        if self.phase != "success" or method != "PUT" or query or len(self.stage_records) >= len(self.stage_names):
            _reject(self.trace, handler, "unexpected staging request")
            return
        expected_name = self.stage_names[len(self.stage_records)]
        expected_path = f"{self._release_upload_prefix()}/staging/files/{quote(expected_name, safe='')}"
        if path != expected_path or body is None:
            _reject(self.trace, handler, "unexpected staging request")
            return
        if unquote(path.rsplit("/", 1)[-1]) != expected_name:
            _reject(self.trace, handler, "unexpected staging path")
            return
        descriptor = next(file for file in _manifest_files(self.manifest) if file["name"] == expected_name)
        file_path = self.root / expected_name
        _regular_file(file_path, "staged release file")
        expected_body = file_path.read_bytes()
        expected_hash = hashlib.sha256(expected_body).hexdigest()
        expected_size = len(expected_body)
        if (
            body != expected_body
            or descriptor["sha256"] != expected_hash
            or descriptor["size_bytes"] != expected_size
            or not _header_is(handler, "Authorization", f"Bearer {TEST_RELEASE_TOKEN}")
            or not _header_is(handler, "Content-Type", "application/octet-stream")
            or not _header_is(handler, "Content-Length", str(expected_size))
            or not _header_is(handler, "X-PortalSurfer-Sha256", expected_hash)
            or not _header_is(handler, "X-PortalSurfer-Release-Version", self.manifest["version"])
            or not _header_is(handler, "X-PortalSurfer-Release-Channel", self.manifest["channel"])
            or not _header_is(handler, "X-PortalSurfer-Released-At", self.manifest["released_at"])
            or not _header_absent(handler, "X-PortalSurfer-Manifest-Sha256")
            or not _header_absent(handler, "X-PortalSurfer-Release-Attestation")
        ):
            _reject(self.trace, handler, "unexpected staging request")
            return
        self.stage_records.append(expected_name)
        self.trace.events.append(f"stage:{expected_name}")
        _send_response(handler, 200, b"staged\n", "text/plain; charset=utf-8")

    def _commit(
        self,
        handler: http.server.BaseHTTPRequestHandler,
        method: str,
        path: str,
        query: str,
        body: bytes | None,
    ) -> None:
        expected_path = f"{self._release_upload_prefix()}/commit"
        manifest_bytes = release_helper.canonical_json(self.manifest)
        manifest_sha = hashlib.sha256(manifest_bytes).hexdigest()
        if (
            self.phase != "success"
            or method != "PUT"
            or path != expected_path
            or query
            or body != manifest_bytes
            or self.stage_records != self.stage_names
            or self.commit_count != 0
            or not _header_is(handler, "Authorization", f"Bearer {TEST_RELEASE_TOKEN}")
            or not _header_is(handler, "Content-Type", MANIFEST_CONTENT_TYPE)
            or not _header_is(handler, "Content-Length", str(len(manifest_bytes)))
            or not _header_is(handler, "X-PortalSurfer-Manifest-Sha256", manifest_sha)
            or not _header_is(handler, "X-PortalSurfer-Release-Version", self.manifest["version"])
            or not _header_is(handler, "X-PortalSurfer-Release-Channel", self.manifest["channel"])
            or not _header_is(handler, "X-PortalSurfer-Released-At", self.manifest["released_at"])
            or not _header_is(handler, "X-PortalSurfer-Release-Attestation", TEST_ATTESTATION_TOKEN)
            or not _header_absent(handler, "X-PortalSurfer-Sha256")
        ):
            _reject(self.trace, handler, "unexpected commit request")
            return
        self.commit_count += 1
        self.trace.events.append("commit")
        _send_json(handler, 200, {"status": "accepted"})

    def handle(self, handler: http.server.BaseHTTPRequestHandler, method: str) -> None:
        parts = _url_parts(handler)
        body = _request_body(handler)
        if parts is None or body is None:
            _reject(self.trace, handler, "malformed API request")
            return
        path, query = parts
        if path == CAPABILITY_PATH:
            self._capability(handler, method, path, query, body)
        elif path.endswith("/commit"):
            self._commit(handler, method, path, query, body)
        else:
            self._stage(handler, method, path, query, body)


class OidcMock:
    def __init__(self, expected_audience: str, trace: Trace) -> None:
        self.expected_audience = expected_audience
        self.trace = trace
        self.request_count = 0

    def handle(self, handler: http.server.BaseHTTPRequestHandler, method: str) -> None:
        parts = _url_parts(handler)
        body = _request_body(handler)
        query = "" if parts is None else parts[1]
        if (
            parts is None
            or method != "GET"
            or parts[0] != "/oidc/token"
            or parse_qs(query, keep_blank_values=True) != {"audience": [self.expected_audience]}
            or body != b""
            or self.request_count != 0
            or not _header_is(handler, "Accept", "application/json")
            or not _header_is(handler, "Authorization", f"bearer {TEST_OIDC_REQUEST_TOKEN}")
        ):
            _reject(self.trace, handler, "unexpected OIDC request")
            return
        self.request_count += 1
        self.trace.events.append("oidc")
        _send_json(handler, 200, {"value": TEST_ATTESTATION_TOKEN})


class StrictRequestHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _dispatch(self, method: str) -> None:
        try:
            self.server.mock.handle(self, method)  # type: ignore[attr-defined]
        except Exception:
            _reject(self.server.mock.trace, self, "mock request validation failed")  # type: ignore[attr-defined]

    def do_DELETE(self) -> None:
        self._dispatch("DELETE")

    def do_GET(self) -> None:
        self._dispatch("GET")

    def do_HEAD(self) -> None:
        self._dispatch("HEAD")

    def do_OPTIONS(self) -> None:
        self._dispatch("OPTIONS")

    def do_PATCH(self) -> None:
        self._dispatch("PATCH")

    def do_POST(self) -> None:
        self._dispatch("POST")

    def do_PUT(self) -> None:
        self._dispatch("PUT")

    def do_TRACE(self) -> None:
        self._dispatch("TRACE")

    def log_message(self, format: str, *args: Any) -> None:
        del format, args


class StrictServer(http.server.HTTPServer):
    def __init__(self, mock: Any) -> None:
        super().__init__(("127.0.0.1", 0), StrictRequestHandler)
        self.mock = mock


def _start_server(mock: Any) -> tuple[StrictServer, threading.Thread, str]:
    server = StrictServer(mock)
    thread = threading.Thread(target=server.serve_forever, name="wave-release-integration-mock", daemon=True)
    thread.start()
    port = server.server_address[1]
    return server, thread, f"http://127.0.0.1:{port}"


def _stop_server(server: StrictServer, thread: threading.Thread) -> None:
    server.shutdown()
    server.server_close()
    thread.join(timeout=2)


def _publisher_environment(*, oidc_url: str | None = None) -> dict[str, str]:
    environment = os.environ.copy()
    for name in (
        "PORTALSURFER_RELEASE_ENDPOINT",
        "PORTALSURFER_RELEASE_TOKEN",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ):
        environment.pop(name, None)
    environment["PORTALSURFER_RELEASE_TOKEN"] = TEST_RELEASE_TOKEN
    if oidc_url is not None:
        environment["ACTIONS_ID_TOKEN_REQUEST_URL"] = oidc_url
        environment["ACTIONS_ID_TOKEN_REQUEST_TOKEN"] = TEST_OIDC_REQUEST_TOKEN
    return environment


def _run_publisher(
    *,
    node: str,
    publisher: Path,
    scratch: Path,
    endpoint: str,
    environment: dict[str, str],
) -> subprocess.CompletedProcess[str]:
    command = [
        node,
        str(publisher),
        "--manifest",
        str(scratch / "release-manifest.json"),
        "--root",
        str(scratch),
        "--endpoint",
        endpoint,
    ]
    try:
        result = subprocess.run(command, cwd=ROOT, env=environment, text=True, capture_output=True, check=False)
    except OSError as error:
        raise HarnessError(f"could not execute the pinned Node publisher: {error}") from error
    for output in (result.stdout, result.stderr):
        require(
            all(secret not in output for secret in (TEST_RELEASE_TOKEN, TEST_OIDC_REQUEST_TOKEN, TEST_ATTESTATION_TOKEN)),
            "publisher output contained a test credential",
        )
    return result


def _verify_publisher_pin(publisher: Path) -> None:
    publisher_repo = publisher.parent.parent
    require(publisher == publisher_repo / "scripts" / "publish-plugin-release.mjs", "publisher script is outside the pinned checkout path")
    try:
        result = subprocess.run(
            ["git", "-C", str(publisher_repo), "rev-parse", "HEAD"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
    except OSError as error:
        raise HarnessError(f"could not inspect the pinned publisher checkout: {error}") from error
    require(result.returncode == 0 and result.stdout.strip() == PUBLISHER_COMMIT, "publisher checkout is not at the required PortalSurfer commit")
    status = subprocess.run(
        ["git", "-C", str(publisher_repo), "status", "--porcelain", "--untracked-files=all"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    require(status.returncode == 0 and not status.stdout, "publisher checkout has local changes")
    tracked = subprocess.run(
        ["git", "-C", str(publisher_repo), "show", f"{PUBLISHER_COMMIT}:scripts/publish-plugin-release.mjs"],
        cwd=ROOT,
        capture_output=True,
        check=False,
    )
    require(tracked.returncode == 0 and tracked.stdout == publisher.read_bytes(), "publisher script is not the pinned file")


def _assert_scratch_has_no_secrets(scratch: Path) -> None:
    for path in scratch.rglob("*"):
        if path.is_file() and not path.is_symlink():
            contents = path.read_bytes()
            require(
                all(secret.encode("utf-8") not in contents for secret in (TEST_RELEASE_TOKEN, TEST_OIDC_REQUEST_TOKEN, TEST_ATTESTATION_TOKEN)),
                "scratch release files contain a test credential",
            )


@contextmanager
def _assembled_release(args: argparse.Namespace) -> Iterator[tuple[dict[str, Any], Path]]:
    release_helper.validate_publication_version(args.package_version, args.publication_version, "nightly")
    _, _, mac_clap, mac_vst3, mac_screenshot, mac_changelog = _load_macos_preflight(
        Path(args.macos_artifact_root),
        package_version=args.package_version,
        build_id=args.build_id,
        source_sha=args.source_sha,
        released_at=args.released_at,
    )
    _, _, windows_archive = _load_windows_artifact(
        Path(args.windows_artifact_root),
        package_version=args.package_version,
        publication_version=args.publication_version,
        build_id=args.build_id,
        source_sha=args.source_sha,
        released_at=args.released_at,
    )

    scratch_parent = Path(args.scratch_parent).resolve() if args.scratch_parent else None
    with tempfile.TemporaryDirectory(prefix="wave-release-integration-", dir=scratch_parent) as directory:
        scratch = Path(directory)
        clap = scratch / f"wave-v{args.publication_version}-macos.clap.zip"
        vst3 = scratch / f"wave-v{args.publication_version}-macos.vst3.zip"
        windows = scratch / windows_archive.name
        screenshot = scratch / "wave-default-960x600.png"
        changelog = scratch / "CHANGELOG.md"
        _copy(mac_clap, clap, "macOS CLAP preflight archive")
        _copy(mac_vst3, vst3, "macOS VST3 preflight archive")
        _copy(windows_archive, windows, "Windows archive")
        _copy(mac_screenshot, screenshot, "macOS preflight screenshot")
        _copy(mac_changelog, changelog, "macOS preflight changelog")

        manifest = release_helper.build_manifest(
            publication_version=args.publication_version,
            package_version=args.package_version,
            build_id=args.build_id,
            channel="nightly",
            released_at=args.released_at,
            git_sha=args.source_sha,
            clap=clap,
            vst3=vst3,
            screenshot=screenshot,
            changelog=changelog,
            distribution="production",
            signing_identity_class="Developer ID Application",
            notarized=True,
            stapled=True,
            signing_team_id=release_helper.WAVE_TEAM_ID,
            notary_submissions={"clap": TEST_NOTARY_ID, "vst3": TEST_NOTARY_ID},
            windows_vst3=windows,
        )
        (scratch / "release-manifest.json").write_bytes(release_helper.canonical_json(manifest))
        release_helper.validate_manifest(manifest, scratch)
        _exact_entries(
            scratch,
            {
                "release-manifest.json",
                clap.name,
                vst3.name,
                windows.name,
                screenshot.name,
                changelog.name,
            },
            "combined release scratch",
        )
        require(manifest["schema_version"] == release_helper.MANIFEST_SCHEMA_V3, "combined WAVE release must use schema 3")
        require(manifest["artifacts"][2]["security"] == {"status": "unsigned", "certificate": None}, "Windows security metadata is invalid")
        require(not (scratch / "windows-artifact-manifest.json").exists(), "Windows sidecar leaked into assembly scratch")
        _assert_scratch_has_no_secrets(scratch)
        yield manifest, scratch


def _require_combined_scratch(scratch: Path, manifest: dict[str, Any], label: str) -> None:
    _exact_entries(scratch, {file["name"] for file in _manifest_files(manifest)} | {"release-manifest.json"}, label)
    _assert_scratch_has_no_secrets(scratch)


def run_artifact_contract(args: argparse.Namespace) -> None:
    with _assembled_release(args) as (manifest, scratch):
        artifacts = manifest["artifacts"]
        mac_security = {
            "status": "signed",
            "certificate": "Developer ID Application",
            "team_id": release_helper.WAVE_TEAM_ID,
            "notarized": True,
            "stapled": True,
            "notary_submission": TEST_NOTARY_ID,
        }
        require(artifacts[0]["security"] == mac_security, "schema 3 CLAP security metadata is invalid")
        require(artifacts[1]["security"] == mac_security, "schema 3 VST3 security metadata is invalid")
        require(artifacts[2]["security"] == {"status": "unsigned", "certificate": None}, "schema 3 Windows security metadata is invalid")
        _require_combined_scratch(scratch, manifest, "combined release scratch")


def run_publisher_integration(args: argparse.Namespace) -> None:
    publisher = Path(args.publisher_script).absolute()
    _regular_file(publisher, "publisher script")
    require(publisher.name == "publish-plugin-release.mjs", "publisher script has an unexpected name")
    _verify_publisher_pin(publisher)

    with _assembled_release(args) as (manifest, scratch):
        _require_combined_scratch(scratch, manifest, "combined release scratch")

        trace = Trace()
        api_mock = ApiMock(manifest, scratch, trace)
        api_server, api_thread, api_endpoint = _start_server(api_mock)
        manifest_bytes = release_helper.canonical_json(manifest)
        audience = f"{api_endpoint}/plugins/api/v1/products/wave/release-attestations/sha256/{hashlib.sha256(manifest_bytes).hexdigest()}"
        oidc_mock = OidcMock(audience, trace)
        oidc_server, oidc_thread, oidc_endpoint = _start_server(oidc_mock)
        try:
            missing_oidc = _run_publisher(
                node=args.node,
                publisher=publisher,
                scratch=scratch,
                endpoint=api_endpoint,
                environment=_publisher_environment(),
            )
            missing_output = missing_oidc.stdout + missing_oidc.stderr
            require(missing_oidc.returncode != 0, "publisher unexpectedly accepted schema 3 without OIDC")
            require(
                "ACTIONS_ID_TOKEN_REQUEST_URL" in missing_output,
                f"publisher failed without the expected OIDC requirement: {_redact(missing_output[-400:])}",
            )
            require(trace.events == ["capability"], "missing-OIDC failure did not stop before staging")
            require(not trace.unexpected and not oidc_mock.request_count, "missing-OIDC run made an unexpected request")
            require(not api_mock.stage_records and api_mock.commit_count == 0, "missing-OIDC run staged or committed files")

            api_mock.reset_for_success()
            oidc_mock.request_count = 0
            successful = _run_publisher(
                node=args.node,
                publisher=publisher,
                scratch=scratch,
                endpoint=api_endpoint,
                environment=_publisher_environment(oidc_url=oidc_endpoint + "/oidc/token"),
            )
            require(successful.returncode == 0, f"publisher transport run failed: {_redact(successful.stderr[-400:])}")
            expected_events = ["capability", *[f"stage:{name}" for name in api_mock.stage_names], "oidc", "commit"]
            require(trace.events == expected_events, "publisher request ordering is invalid")
            require(not trace.unexpected, "strict mock rejected an expected publisher request")
            require(api_mock.stage_records == api_mock.stage_names, "publisher did not stage the expected files")
            require(api_mock.commit_count == 1, "publisher did not perform exactly one final commit")
            require(oidc_mock.request_count == 1, "publisher did not perform exactly one OIDC request")
            _require_combined_scratch(scratch, manifest, "combined release scratch after publisher integration")
        finally:
            _stop_server(oidc_server, oidc_thread)
            _stop_server(api_server, api_thread)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("artifact-contract", "publisher-integration"), required=True)
    parser.add_argument("--macos-artifact-root", required=True)
    parser.add_argument("--windows-artifact-root", required=True)
    parser.add_argument("--publisher-script", help="Pinned Node publisher script; required for publisher-integration")
    parser.add_argument("--package-version", required=True)
    parser.add_argument("--publication-version", required=True)
    parser.add_argument("--build-id", required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--released-at", required=True)
    parser.add_argument("--node", default="node", help="Node executable used for the pinned publisher")
    parser.add_argument("--scratch-parent")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    try:
        if args.mode == "artifact-contract":
            require(args.publisher_script is None, "--publisher-script is only valid for publisher-integration")
            run_artifact_contract(args)
        else:
            require(args.publisher_script is not None, "--publisher-script is required for publisher-integration")
            run_publisher_integration(args)
    except (HarnessError, OSError, ValueError) as error:
        print(f"release pipeline integration failed: {_redact(str(error))}", file=sys.stderr)
        return 1
    if args.mode == "artifact-contract":
        print("artifact contract ok: WAVE macOS preflight and Windows sidecar assembled schema 3 with an exact six-file scratch set.")
    else:
        print("publisher integration ok: schema 3 staged through strict loopback API/OIDC mocks; 5 files staged, 1 commit.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
