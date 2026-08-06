import json
import os
import shutil
import struct
import subprocess
import tempfile
import unittest
from unittest import mock
import zlib
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))
import release_helper
import bump_version


def png(width=960, height=600):
    def chunk(kind, payload):
        import zlib
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
    scanlines = b"".join(b"\x00" + bytes(width * 3) for _ in range(height))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(scanlines)) + chunk(b"IEND", b"")


class FakeTransport:
    """Deterministic in-memory PortalSurfer transport; never opens a socket."""

    def __init__(self, manifest_schema_versions=(2,)):
        self.manifest_schema_versions = list(manifest_schema_versions)
        self.calls = []

    def __call__(self, url, method, body, headers):
        self.calls.append((url, method, body, headers))
        if method == "GET":
            payload = json.dumps({"release_upload": {"manifest_schema_versions": self.manifest_schema_versions}}).encode()
            return 200, payload
        return 201, b""


class ReleaseHelperTests(unittest.TestCase):
    def test_wave_release_identifier_and_artifact_contract(self):
        root = Path(__file__).parents[1]
        workflow = (root / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
        release_script = (root / "scripts" / "release.sh").read_text(encoding="utf-8")
        helper = (root / "scripts" / "release_helper.py").read_text(encoding="utf-8")

        self.assertIn("secrets.WAVE_RELEASE_UPLOAD_TOKEN", workflow)
        self.assertIn("https://portalsurfer.org/plugins/api/v1/products/wave/releases", workflow)
        self.assertIn("target/ui-screenshots/wave/initial-ui-default.png", release_script)
        self.assertIn('CFBundleName</key><string>WAVE</string>', release_script)
        self.assertIn('wave-v${version}-macos.clap.zip', release_script)
        self.assertIn('wave-v${version}-macos.vst3.zip', release_script)
        self.assertIn("wave-default-960x600.png", release_script)
        self.assertIn("wave-v{manifest['version']}-macos.clap.zip", helper)
        self.assertIn("wave-v{manifest['version']}-macos.vst3.zip", helper)
        self.assertIn('com.portalsurfer.wave.', helper)
        self.assertIn('"product": "wave"', helper)

        release_files = [
            root / ".git-cliff.toml",
            root / ".github" / "workflows" / "changelog.yml",
            root / ".github" / "workflows" / "nightly.yml",
            root / ".github" / "workflows" / "release-preflight.yml",
            root / ".github" / "workflows" / "release.yml",
            root / "scripts" / "bump_version.py",
            root / "scripts" / "release.sh",
            root / "scripts" / "release_helper.py",
            root / "scripts" / "update_changelog.sh",
            root / "CHANGELOG.md",
            root / "README.md",
        ]
        forbidden_identifier = "pu" + "mp"
        for path in release_files:
            self.assertNotIn(forbidden_identifier, path.read_text(encoding="utf-8").lower(), path)

    def test_nightly_scheduler_contract(self):
        workflow = (Path(__file__).parents[1] / ".github" / "workflows" / "nightly.yml").read_text(encoding="utf-8")
        self.assertIn('cron: "0 20 * * *"', workflow)
        self.assertIn('timezone: "Europe/Amsterdam"', workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("type: boolean", workflow)
        self.assertIn("actions: write", workflow)
        self.assertNotIn("PORTALSURFER_RELEASE_TOKEN", workflow)
        self.assertNotIn("RADIANT_REPO_TOKEN", workflow)
        self.assertNotIn("APPLE_", workflow)
        self.assertNotIn("git ls-remote", workflow)
        self.assertNotIn("PORTALSURFER_RELEASES_URL", workflow)
        self.assertNotIn("curl --fail --silent --show-error --location --max-time", workflow)
        self.assertNotIn("jq", workflow)
        self.assertNotIn("latest_nightly_sha", workflow)
        self.assertIn("actions/workflows/release.yml/dispatches", workflow)
        self.assertIn('\\"channel\\":\\"nightly\\"', workflow)
        self.assertIn('\\"publish\\":\\"true\\"', workflow)
        self.assertIn('\\"only_if_changed\\":\\"${only_if_changed}\\"', workflow)

    def test_nightly_release_decision_behavior(self):
        sha_a = "a" * 40
        sha_b = "b" * 40
        document = {
            "releases": [
                {"channel": "stable", "released_at": "2026-07-29T23:00:00+02:00", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha_b}},
                {"channel": "nightly", "released_at": "2026-07-29T20:00:00+02:00", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha_a}},
                {"channel": "nightly", "released_at": "2026-07-29T18:30:00Z", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha_b}},
            ]
        }
        self.assertFalse(release_helper.should_release(source_sha=sha_b, document=document))
        self.assertTrue(release_helper.should_release(source_sha=sha_a, document=document))
        self.assertEqual(release_helper.latest_release_source_sha(document, channel="nightly"), sha_b)
        self.assertIsNone(release_helper.latest_release_source_sha({"releases": []}, channel="nightly"))
        self.assertTrue(release_helper.should_release(source_sha=sha_a, document={"releases": []}))

    def test_release_version_advances_globally_across_channels(self):
        document = {
            "releases": [
                {"channel": "nightly", "version": "0.2.3", "released_at": "2026-07-29T20:00:00Z"},
                {"channel": "stable", "version": "0.2.5", "released_at": "2026-07-30T20:00:00Z"},
            ]
        }
        self.assertEqual(release_helper.latest_release_version(document), "0.2.5")
        self.assertEqual(release_helper.next_release_version("0.2.0", document), "0.2.6")
        self.assertEqual(release_helper.next_release_version("0.2.6", document), "0.2.6")

    def test_release_version_rejects_malformed_history(self):
        with self.assertRaisesRegex(ValueError, "release version"):
            release_helper.latest_release_version({"releases": [{"channel": "nightly", "version": "0.2.x"}]})

    def test_bump_version_updates_manifest_and_lockfile(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "Cargo.toml"
            lockfile = root / "Cargo.lock"
            manifest.write_text('[package]\nname = "wave"\nversion = "0.2.0"\n\n[dependencies]\n', encoding="utf-8")
            lockfile.write_text('[[package]]\nname = "wave"\nversion = "0.2.0"\n', encoding="utf-8")
            self.assertEqual(bump_version.bump_package_version(manifest, lockfile, "0.2.1"), "0.2.0")
            self.assertIn('version = "0.2.1"', manifest.read_text(encoding="utf-8"))
            self.assertIn('version = "0.2.1"', lockfile.read_text(encoding="utf-8"))

    def test_release_workflow_commits_version_before_build(self):
        workflow = (Path(__file__).parents[1] / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
        self.assertIn("contents: write", workflow)
        self.assertIn("group: wave-release", workflow)
        self.assertIn("release_helper.next_release_version", workflow)
        self.assertIn("python3 scripts/bump_version.py", workflow)
        self.assertIn("git push origin HEAD:main", workflow)
        self.assertIn("name: wave-release-${{ inputs.channel }}-${{ steps.release_source.outputs.sha }}", workflow)

    def test_release_decision_preserves_requested_channel(self):
        sha_a = "a" * 40
        sha_b = "b" * 40
        document = {
            "releases": [
                {"channel": "stable", "released_at": "2026-07-29T20:00:00Z", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha_a}},
                {"channel": "rc", "released_at": "2026-07-29T20:00:00Z", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha_b}},
            ]
        }
        self.assertFalse(release_helper.should_release(source_sha=sha_a, document=document, channel="stable"))
        self.assertTrue(release_helper.should_release(source_sha=sha_b, document=document, channel="stable"))
        self.assertFalse(release_helper.should_release(source_sha=sha_b, document=document, channel="rc"))
        self.assertTrue(release_helper.should_release(source_sha=sha_a, document=document, channel="rc"))

    def test_release_decision_uses_parsed_timezone_aware_timestamp(self):
        sha_old = "a" * 40
        sha_new = "b" * 40
        document = {
            "releases": [
                {"channel": "nightly", "released_at": "2026-07-29T20:00:00+02:00", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha_old}},
                {"channel": "nightly", "released_at": "2026-07-29T18:30:00Z", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha_new}},
            ]
        }
        self.assertEqual(release_helper.latest_release_source_sha(document, channel="nightly"), sha_new)
        malformed = {"releases": [{"channel": "nightly", "released_at": "2026-07-29T20:00:00", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha_old}}]}
        with self.assertRaisesRegex(ValueError, "timezone"):
            release_helper.latest_release_source_sha(malformed, channel="nightly")

    def test_release_decision_rejects_malformed_history(self):
        sha = "a" * 40
        bad_documents = [
            None,
            {"releases": "not-an-array"},
            {"releases": [None]},
            {"releases": [{"channel": "nightly", "released_at": "2026-07-29T20:00:00Z", "source": {"repository": "PORTALSURFER/wave", "git_sha": "bad"}}]},
            {"releases": [{"channel": "nightly", "released_at": "not-a-date", "source": {"repository": "PORTALSURFER/wave", "git_sha": sha}}]},
            {"releases": [{"channel": "nightly", "released_at": "2026-07-29T20:00:00Z", "source": {"repository": "other/repo", "git_sha": sha}}]},
        ]
        for document in bad_documents:
            with self.subTest(document=document), self.assertRaises(ValueError):
                release_helper.should_release(source_sha=sha, document=document)

    def test_release_decision_bypass_does_not_require_or_query_history(self):
        with mock.patch.object(release_helper, "latest_release_source_sha", side_effect=AssertionError("history queried")):
            self.assertTrue(release_helper.should_release(source_sha="a" * 40, only_if_changed=False))

    def test_release_workflow_gate_and_downstream_contract(self):
        workflow = (Path(__file__).parents[1] / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
        self.assertIn("only_if_changed:", workflow)
        checkout = workflow.index("- name: Checkout exact main source")
        capture = workflow.index("- name: Capture checked-out source SHA")
        gate = workflow.index("- name: Check release source freshness")
        install = workflow.index("- name: Install Rust")
        self.assertLess(checkout, capture)
        self.assertLess(capture, gate)
        self.assertLess(gate, install)
        self.assertNotIn("RADIANT_REPO_TOKEN", workflow)
        self.assertIn("RELEASES_URL: https://portalsurfer.org/plugins/api/v1/products/wave/releases", workflow)
        self.assertIn("release_helper.should_release", workflow)
        self.assertIn("RELEASE_CHANNEL: ${{ inputs.channel }}", workflow)
        gate_block = workflow[gate:install]
        self.assertIn("source_sha, releases_path, output_path, channel = sys.argv[1:]", gate_block)
        self.assertIn("channel=channel", gate_block)
        self.assertNotIn('channel="nightly"', gate_block)
        self.assertIn("if: steps.gate.outputs.should_release == 'true'", workflow)
        self.assertEqual(workflow.count("if: steps.gate.outputs.should_release == 'true'"), 6)
        self.assertIn("group: wave-release", workflow)

    def test_release_preflight_covers_workflow_and_helper_changes(self):
        workflow = (Path(__file__).parents[1] / ".github" / "workflows" / "release-preflight.yml").read_text(encoding="utf-8")
        for path in (".github/workflows/release.yml", ".github/workflows/nightly.yml", "scripts/release_helper.py", "scripts/bump_version.py", "tests/release_helper_test.py"):
            self.assertIn(path, workflow)
        self.assertIn("python3 tests/release_helper_test.py", workflow)

    def test_release_workflow_artifact_uses_released_source_sha(self):
        workflow = (Path(__file__).parents[1] / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
        checkout = workflow.index("- name: Checkout exact main source")
        capture = workflow.index("- name: Capture checked-out source SHA")
        release_capture = workflow.index("- name: Capture release source SHA")
        upload = workflow.index("- name: Upload immutable bundle for inspection")
        self.assertLess(checkout, capture)
        self.assertLess(capture, release_capture)
        self.assertLess(capture, upload)
        capture_block = workflow[capture:release_capture]
        self.assertIn("id: source_sha", capture_block)
        self.assertIn('git rev-parse HEAD', capture_block)
        upload_block = workflow[upload:]
        self.assertIn("name: wave-release-${{ inputs.channel }}-${{ steps.release_source.outputs.sha }}", upload_block)
        self.assertNotIn("${{ github.sha }}", upload_block)

    def test_manifest_and_png_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            screenshot = root / "wave-default-960x600.png"
            screenshot.write_bytes(png())
            clap, vst3, changelog = (root / name for name in ("clap.zip", "vst3.zip", "CHANGELOG.md"))
            clap.write_bytes(b"clap")
            vst3.write_bytes(b"vst3")
            changelog.write_text("# Release\n", encoding="utf-8")
            manifest = release_helper.build_manifest(version="0.2.0", build_id="wave-v0.2.0-abcdef012345", channel="stable", released_at="2026-07-28T00:00:00Z", git_sha="a" * 40, clap=clap, vst3=vst3, screenshot=screenshot, changelog=changelog, distribution="production", signing_identity_class="Developer ID Application", notarized=True, stapled=True, signing_team_id="TEAM123456", notary_submissions={"clap": "12345678-1234-4123-8123-123456789abc", "vst3": "abcdefab-cdef-4abc-8def-abcdefabcdef"})
            self.assertEqual(manifest["schema_version"], 2)
            self.assertEqual(manifest["source"]["dirty"], False)
            self.assertEqual((manifest["screenshot"]["logical_width"], manifest["screenshot"]["logical_height"]), (960, 600))
            self.assertEqual(json.loads(release_helper.canonical_json(manifest)), manifest)

    def test_preflight_manifest_uses_ad_hoc_non_notarized_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            screenshot = root / "wave-default-960x600.png"
            screenshot.write_bytes(png())
            clap, vst3, changelog = (root / name for name in ("wave-v0.2.0-macos.clap.zip", "wave-v0.2.0-macos.vst3.zip", "CHANGELOG.md"))
            clap.write_bytes(b"clap")
            vst3.write_bytes(b"vst3")
            changelog.write_text("# Release\n", encoding="utf-8")
            manifest = release_helper.build_manifest(version="0.2.0", build_id="wave-v0.2.0-abcdef012345", channel="stable", released_at="2026-07-29T00:00:00Z", git_sha="a" * 40, clap=clap, vst3=vst3, screenshot=screenshot, changelog=changelog, distribution="preflight", signing_identity_class="ad hoc", notarized=False, stapled=False)
            release_helper.validate_preflight_manifest(manifest, root)
            self.assertEqual(manifest["signing"], {"identity_class": "ad hoc", "notarized": False, "stapled": False, "team_id": "", "notary_submissions": {}})
            with self.assertRaisesRegex(ValueError, "production Developer ID notarized"):
                release_helper.validate_publish_manifest(manifest, root)

    def test_release_preflight_cli_contract_is_credential_free(self):
        script = (Path(__file__).parents[1] / "scripts" / "release.sh").read_text(encoding="utf-8")
        self.assertIn("--preflight", script)
        self.assertIn('codesign --force --deep --sign - "${bundle_dir}"', script)
        self.assertIn('validate_preflight_manifest', script)
        preflight = script[script.index('if [[ "${mode}" == preflight ]]; then'):script.index('else', script.index('if [[ "${mode}" == preflight ]]; then'))]
        self.assertNotIn("APPLE_DEVELOPER_ID_APPLICATION_CERT_BASE64", preflight)
        self.assertIn("require_production=False", script)
        self.assertLess(script.index("printf 'preflight CodeResources\\n'"), script.index('codesign --force --deep --sign - "${bundle_dir}"'))

    def test_release_vst3_gate_precedes_packaging(self):
        script = (Path(__file__).parents[1] / "scripts" / "release.sh").read_text(encoding="utf-8")
        sdk_validation = 'if [[ ! -d "${VST3_SDK_DIR:-}" ]]'
        vst3_gate = "bash scripts/ci.sh --vst3"
        screenshots = "bash scripts/ci.sh --screenshots"
        clap_packaging = 'TOYBOX_ACTIVE_ARTIFACT=clap CARGO_TARGET_DIR="${clap_target}" cargo build --locked --release'
        vst3_packaging = 'TOYBOX_ACTIVE_ARTIFACT=vst3 VST3_SDK_DIR="${VST3_SDK_DIR}" CARGO_TARGET_DIR="${vst3_target}" cargo rustc --locked --release --features vst3 -- -C link-arg=-Wl,-bundle'
        self.assertIn(vst3_gate, script)
        self.assertIn(screenshots, script)
        self.assertIn(clap_packaging, script)
        self.assertIn(vst3_packaging, script)
        self.assertLess(script.index(sdk_validation), script.index(vst3_gate))
        self.assertLess(script.index(vst3_gate), script.index(vst3_packaging))
        self.assertLess(script.index(screenshots), script.index(clap_packaging))
        self.assertLess(script.index(screenshots), script.index(vst3_packaging))

    def test_capability_refusal_makes_zero_puts(self):
        transport = FakeTransport(manifest_schema_versions=(1,))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            names = ["a.zip", "b.zip", "shot.png", "CHANGELOG.md"]
            for name in names:
                (root / name).write_bytes(png() if name == "shot.png" else name.encode())
            manifest = release_helper.build_manifest(version="0.2.0", build_id="wave-v0.2.0-test", channel="stable", released_at="2026-07-28T00:00:00Z", git_sha="a" * 40, clap=root / "a.zip", vst3=root / "b.zip", screenshot=root / "shot.png", changelog=root / "CHANGELOG.md", distribution="production", signing_identity_class="Developer ID Application", notarized=True, stapled=True, signing_team_id="TEAM123456", notary_submissions={"clap": "12345678-1234-4123-8123-123456789abc", "vst3": "abcdefab-cdef-4abc-8def-abcdefabcdef"})
            with self.assertRaisesRegex(RuntimeError, "schema 2"):
                release_helper._publish_validated_manifest(endpoint=release_helper.PRODUCTION_ORIGIN, token="secret", manifest=manifest, root=root, transport=transport)
        self.assertEqual([call[1] for call in transport.calls], ["GET"])

    def test_v2_uploads_four_files_and_exact_manifest_commit(self):
        transport = FakeTransport()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = []
            for name, body in (("a.zip", b"a"), ("b.zip", b"b"), ("shot.png", png()), ("CHANGELOG.md", b"c")):
                (root / name).write_bytes(body); files.append((name, root / name, release_helper.file_digest(root / name)[0]))
            manifest = {"schema_version": 2, "product": "wave", "build_id": "wave-v0.2.0-test", "source": {"repository": "PORTALSURFER/wave", "git_sha": "a" * 40, "dirty": False}, "distribution": "production", "signing": {"identity_class": "Developer ID Application", "notarized": True, "stapled": True, "team_id": "TEAM123456", "notary_submissions": {"clap": "12345678-1234-4123-8123-123456789abc", "vst3": "abcdefab-cdef-4abc-8def-abcdefabcdef"}}, "version": "0.2.0", "channel": "stable", "released_at": "2026-07-28T00:00:00Z", "artifacts": [{"format": "clap", "platform": "macos", "architectures": ["arm64"], "name": "a.zip", "sha256": files[0][2], "size_bytes": (root / "a.zip").stat().st_size, "media_type": "application/zip"}, {"format": "vst3", "platform": "macos", "architectures": ["arm64"], "name": "b.zip", "sha256": files[1][2], "size_bytes": (root / "b.zip").stat().st_size, "media_type": "application/zip"}], "screenshot": {"role": "default-ui", "name": "shot.png", "media_type": "image/png", "width": 720, "height": 540, "logical_width": 720, "logical_height": 540, "dpi_scale": 1.0, "source_git_sha": "a" * 40, "sha256": files[2][2], "size_bytes": (root / "shot.png").stat().st_size}, "changelog": {"name": "CHANGELOG.md", "format": "markdown", "media_type": "text/markdown; charset=utf-8", "sha256": files[3][2], "size_bytes": (root / "CHANGELOG.md").stat().st_size}}
            release_helper._publish_validated_manifest(endpoint=release_helper.PRODUCTION_ORIGIN, token="secret", manifest=manifest, root=root, transport=transport)
        self.assertEqual(len(transport.calls), 6)
        self.assertEqual(transport.calls[-1][3]["Content-Type"], release_helper.MANIFEST_CONTENT_TYPE)
        self.assertEqual(transport.calls[-1][3]["X-PortalSurfer-Release-Version"], "0.2.0")
        self.assertEqual(transport.calls[-1][3]["X-PortalSurfer-Released-At"], "2026-07-28T00:00:00Z")
        self.assertEqual(transport.calls[-1][2], release_helper.canonical_json(manifest))

    def test_publish_rejects_invalid_endpoints_before_transport(self):
        invalid_endpoints = (
            "http://portalsurfer.org",
            "https://portalsurfer.org/",
            "https://portalsurfer.org:443",
            "https://user@portalsurfer.org",
            "https://portalsurfer.org/path",
        )
        for endpoint in invalid_endpoints:
            with self.subTest(endpoint=endpoint):
                transport = FakeTransport()
                with self.assertRaisesRegex(ValueError, "exact origin https://portalsurfer.org"):
                    release_helper.publish_release(endpoint=endpoint, token="secret", manifest_path=Path("missing.json"), root=Path("."), repo_root=Path("."))

    def test_publish_block_audits_exact_zips_before_manifest_publish(self):
        script = (Path(__file__).parents[1] / "scripts" / "release.sh").read_text(encoding="utf-8")
        publish_block = script.split('if [[ "${mode}" == publish ]]; then', 1)[1]
        self.assertIn("from release_helper import publish_release", publish_block)
        self.assertNotIn("publish_manifest", publish_block)
        self.assertNotIn("final audit of exact publish bytes", publish_block)

    def test_release_build_temp_parent_is_created_before_mktemp(self):
        script = (Path(__file__).parents[1] / "scripts" / "release.sh").read_text(encoding="utf-8")
        parent_creation = 'mkdir -p "${repo_root}/target"'
        temp_creation = 'tmp_root="$(mktemp -d "${repo_root}/target/release-build.XXXXXX")"'
        self.assertLess(script.index(parent_creation), script.index(temp_creation))

    def test_release_and_ci_cargo_invocations_are_lockfile_stable(self):
        root = Path(__file__).parents[1]
        for name in ("scripts/release.sh", "scripts/ci.sh"):
            with self.subTest(script=name):
                lines = (root / name).read_text(encoding="utf-8").splitlines()
                cargo_lines = [
                    line
                    for line in lines
                    if any(
                        f"cargo {command}" in line
                        for command in ("clippy", "test", "build", "rustc")
                    )
                ]
                self.assertTrue(cargo_lines)
                self.assertTrue(
                    all("--locked" in line for line in cargo_lines),
                    f"every Cargo invocation in {name} must use --locked",
                )

    def test_release_script_disables_python_bytecode_output(self):
        root = Path(__file__).parents[1]
        script = (root / "scripts" / "release.sh").read_text(encoding="utf-8")
        self.assertIn("export PYTHONDONTWRITEBYTECODE=1", script)
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            shutil.copy2(root / "scripts" / "release_helper.py", temporary / "release_helper.py")
            environment = os.environ.copy()
            environment["PYTHONPATH"] = str(temporary)
            environment["PYTHONDONTWRITEBYTECODE"] = "1"
            subprocess.run(
                [sys.executable, "-c", "import release_helper"],
                cwd=temporary,
                env=environment,
                check=True,
            )
            self.assertFalse(list(temporary.rglob("*.pyc")))

    def test_release_keychain_is_registered_and_restored(self):
        script = (Path(__file__).parents[1] / "scripts" / "release.sh").read_text(encoding="utf-8")
        capture = "security list-keychains -d user | sed 's/[[:space:]]*\"//g; s/\"$//' > \"${original_keychains_file}\""
        register = 'security list-keychains -d user -s "${release_keychain}" "${original_keychains[@]}" >/dev/null'
        restore = 'security list-keychains -d user -s "${original_keychains[@]}" >/dev/null 2>&1 || true'
        self.assertIn(capture, script)
        self.assertIn(register, script)
        self.assertIn(restore, script)
        self.assertLess(script.index(capture), script.index('security create-keychain'))
        self.assertLess(script.index('security create-keychain'), script.index(register))
        self.assertLess(script.index(register), script.index('security import'))
        self.assertLess(script.index(register), script.index('codesign_identity='))
        self.assertLess(script.index(restore), script.index('security delete-keychain'))

    def test_release_notarization_checks_cover_live_and_extracted_bundles(self):
        script = (Path(__file__).parents[1] / "scripts" / "release.sh").read_text(encoding="utf-8")
        check = 'codesign -vvvv -R=notarized --check-notarization'
        self.assertEqual(script.count(check), 2)
        self.assertNotIn("spctl", script)
        live = script.index(check)
        extracted = script.index(check, live + 1)
        self.assertLess(script.index('xcrun stapler validate "${bundle_dir}"'), live)
        self.assertLess(script.index('xcrun stapler validate "${bundle}"'), extracted)
        self.assertLess(live, script.index('local team_id', live))
        self.assertLess(extracted, script.index('codesign_details=', extracted))

    def test_publish_rejects_tampered_final_zip_before_transport(self):
        transport = FakeTransport()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            screenshot = root / "shot.png"; screenshot.write_bytes(png())
            clap = root / "a.zip"; clap.write_bytes(b"clap")
            vst3 = root / "b.zip"; vst3.write_bytes(b"vst3")
            changelog = root / "CHANGELOG.md"; changelog.write_text("release\n", encoding="utf-8")
            manifest = release_helper.build_manifest(version="0.2.0", build_id="wave-v0.2.0-test", channel="stable", released_at="2026-07-28T00:00:00Z", git_sha="a" * 40, clap=clap, vst3=vst3, screenshot=screenshot, changelog=changelog, distribution="production", signing_identity_class="Developer ID Application", notarized=True, stapled=True, signing_team_id="TEAM123456", notary_submissions={"clap": "12345678-1234-4123-8123-123456789abc", "vst3": "abcdefab-cdef-4abc-8def-abcdefabcdef"})
            clap.write_bytes(b"tampered")
            (root / "release-manifest.json").write_bytes(release_helper.canonical_json(manifest))
            with self.assertRaisesRegex(ValueError, "on-disk bytes do not match manifest"):
                with mock.patch.object(release_helper, "_audit_zip", side_effect=ValueError("ZIP audit failed")), mock.patch.object(release_helper, "_validate_canonical_source"):
                    release_helper.publish_release(endpoint=release_helper.PRODUCTION_ORIGIN, token="secret", manifest_path=root / "release-manifest.json", root=root, repo_root=root)
        self.assertEqual(transport.calls, [])

    def test_public_wrapper_rejects_raw_zip_without_request(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            screenshot = root / "wave-default-960x600.png"; screenshot.write_bytes(png())
            clap = root / "wave-v0.2.0-macos.clap.zip"; clap.write_bytes(b"raw unsigned bytes")
            vst3 = root / "wave-v0.2.0-macos.vst3.zip"; vst3.write_bytes(b"raw unsigned bytes")
            changelog = root / "CHANGELOG.md"; changelog.write_text("release\n", encoding="utf-8")
            manifest = release_helper.build_manifest(version="0.2.0", build_id="wave-v0.2.0-test", channel="stable", released_at="2026-07-28T00:00:00Z", git_sha="a" * 40, clap=clap, vst3=vst3, screenshot=screenshot, changelog=changelog, distribution="production", signing_identity_class="Developer ID Application", notarized=True, stapled=True, signing_team_id="TEAM123456", notary_submissions={"clap": "12345678-1234-4123-8123-123456789abc", "vst3": "abcdefab-cdef-4abc-8def-abcdefabcdef"})
            manifest_path = root / "release-manifest.json"
            manifest_path.write_bytes(release_helper.canonical_json(manifest))
            requests = []
            with self.assertRaisesRegex(ValueError, "ZIP audit failed"), mock.patch.object(release_helper, "_request", side_effect=lambda *args: requests.append(args)), mock.patch.object(release_helper, "_validate_canonical_source"), mock.patch.object(release_helper, "_audit_zip", side_effect=ValueError("ZIP audit failed")):
                release_helper.publish_release(endpoint=release_helper.PRODUCTION_ORIGIN, token="secret", manifest_path=manifest_path, root=root, repo_root=root)
            self.assertEqual(requests, [])

    def test_zip_audit_runs_argument_safe_mac_checks_in_order(self):
        helper_source = (Path(__file__).parents[1] / "scripts" / "release_helper.py").read_text(encoding="utf-8")
        self.assertNotIn("spctl", helper_source)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "wave-v0.2.0-macos.clap.zip"
            archive.write_bytes(b"placeholder")
            calls = []

            def run(args, *, cwd, capture_output=False):
                calls.append(tuple(args))
                if args[0] == "/usr/bin/ditto":
                    extracted = Path(args[-1])
                    binary = extracted / "wave.clap" / "Contents" / "MacOS" / "wave"
                    binary.parent.mkdir(parents=True)
                    (binary.parent.parent / "Info.plist").write_text("plist", encoding="utf-8")
                    (binary.parent.parent / "PkgInfo").write_text("BNDL????", encoding="utf-8")
                    (binary.parent.parent / "CodeResources").write_text("signed resources", encoding="utf-8")
                    binary.write_bytes(b"arm64")
                    os.chmod(binary, 0o755)
                    return subprocess.CompletedProcess(args, 0, "", "")
                if args[0] == "/usr/bin/plutil" and "CFBundleIdentifier" in args:
                    return subprocess.CompletedProcess(args, 0, "com.portalsurfer.wave.clap\n", "")
                if args[0] == "/usr/bin/plutil" and "CFBundlePackageType" in args:
                    return subprocess.CompletedProcess(args, 0, "BNDL\n", "")
                if args[0] == "codesign" and args[1] == "-dv":
                    return subprocess.CompletedProcess(args, 0, "", "Authority=Developer ID Application: PORTALSURFER\nTeamIdentifier=TEAM123456\n")
                if args[0] == "lipo":
                    return subprocess.CompletedProcess(args, 0, "arm64\n", "")
                if args[0] == "/usr/bin/nm":
                    return subprocess.CompletedProcess(args, 0, "_clap_entry\n", "")
                return subprocess.CompletedProcess(args, 0, "", "")

            with mock.patch.object(release_helper.platform, "system", return_value="Darwin"), mock.patch.object(release_helper, "_run_checked", side_effect=run):
                release_helper._audit_zip(archive, "clap", "TEAM123456", cwd=root)
            self.assertEqual([call[0] for call in calls], ["/usr/bin/ditto", "/usr/bin/plutil", "/usr/bin/plutil", "/usr/bin/plutil", "codesign", "codesign", "xcrun", "codesign", "lipo", "/usr/bin/nm"])
            self.assertEqual(calls[7][1:4], ("-vvvv", "-R=notarized", "--check-notarization"))

    def test_zip_audit_accepts_direct_code_resources_for_vst3(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "wave-v0.2.0-macos.vst3.zip"
            archive.write_bytes(b"placeholder")

            def run(args, *, cwd, capture_output=False):
                if args[0] == "/usr/bin/ditto":
                    extracted = Path(args[-1])
                    binary = extracted / "wave.vst3" / "Contents" / "MacOS" / "wave"
                    binary.parent.mkdir(parents=True)
                    (binary.parent.parent / "Info.plist").write_text("plist", encoding="utf-8")
                    (binary.parent.parent / "PkgInfo").write_text("BNDL????", encoding="utf-8")
                    (binary.parent.parent / "CodeResources").write_text("signed resources", encoding="utf-8")
                    binary.write_bytes(b"arm64")
                    os.chmod(binary, 0o755)
                if args[0] == "/usr/bin/plutil" and "CFBundleIdentifier" in args:
                    return subprocess.CompletedProcess(args, 0, "com.portalsurfer.wave.vst3\n", "")
                if args[0] == "/usr/bin/plutil" and "CFBundlePackageType" in args:
                    return subprocess.CompletedProcess(args, 0, "BNDL\n", "")
                if args[0] == "codesign" and args[1] == "-dv":
                    return subprocess.CompletedProcess(args, 0, "", "Authority=Developer ID Application: PORTALSURFER\nTeamIdentifier=TEAM123456\n")
                if args[0] == "lipo":
                    return subprocess.CompletedProcess(args, 0, "arm64\n", "")
                if args[0] == "/usr/bin/nm":
                    return subprocess.CompletedProcess(args, 0, "_GetPluginFactory\n_bundleEntry\n_bundleExit\n", "")
                return subprocess.CompletedProcess(args, 0, "", "")

            with mock.patch.object(release_helper.platform, "system", return_value="Darwin"), mock.patch.object(release_helper, "_run_checked", side_effect=run):
                release_helper._audit_zip(archive, "vst3", "TEAM123456", cwd=root)

    def test_preflight_zip_audit_accepts_direct_code_resources_for_vst3(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "wave-v0.2.0-macos.vst3.zip"
            archive.write_bytes(b"placeholder")

            def run(args, *, cwd, capture_output=False):
                if args[0] == "/usr/bin/ditto":
                    extracted = Path(args[-1])
                    binary = extracted / "wave.vst3" / "Contents" / "MacOS" / "wave"
                    binary.parent.mkdir(parents=True)
                    (binary.parent.parent / "Info.plist").write_text("plist", encoding="utf-8")
                    (binary.parent.parent / "PkgInfo").write_text("BNDL????", encoding="utf-8")
                    (binary.parent.parent / "CodeResources").write_text("preflight signed resources", encoding="utf-8")
                    binary.write_bytes(b"arm64")
                    os.chmod(binary, 0o755)
                if args[0] == "/usr/bin/plutil" and "CFBundleIdentifier" in args:
                    return subprocess.CompletedProcess(args, 0, "com.portalsurfer.wave.vst3\n", "")
                if args[0] == "/usr/bin/plutil" and "CFBundlePackageType" in args:
                    return subprocess.CompletedProcess(args, 0, "BNDL\n", "")
                if args[0] == "lipo":
                    return subprocess.CompletedProcess(args, 0, "arm64\n", "")
                if args[0] == "/usr/bin/nm":
                    return subprocess.CompletedProcess(args, 0, "_GetPluginFactory\n_bundleEntry\n_bundleExit\n", "")
                return subprocess.CompletedProcess(args, 0, "", "")

            with mock.patch.object(release_helper.platform, "system", return_value="Darwin"), mock.patch.object(release_helper, "_run_checked", side_effect=run):
                release_helper._audit_zip(archive, "vst3", "", cwd=root, require_production=False)

    def test_zip_audit_rejects_vst3_code_resources_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "wave-v0.2.0-macos.vst3.zip"
            archive.write_bytes(b"placeholder")

            def run(args, *, cwd, capture_output=False):
                if args[0] == "/usr/bin/ditto":
                    extracted = Path(args[-1])
                    binary = extracted / "wave.vst3" / "Contents" / "MacOS" / "wave"
                    binary.parent.mkdir(parents=True)
                    (binary.parent.parent / "Info.plist").write_text("plist", encoding="utf-8")
                    (binary.parent.parent / "PkgInfo").write_text("BNDL????", encoding="utf-8")
                    (binary.parent.parent / "CodeResources").mkdir()
                    binary.write_bytes(b"arm64")
                    os.chmod(binary, 0o755)
                return subprocess.CompletedProcess(args, 0, "", "")

            with self.assertRaisesRegex(ValueError, r"vst3 ZIP Contents/CodeResources must be a regular file"), mock.patch.object(release_helper.platform, "system", return_value="Darwin"), mock.patch.object(release_helper, "_run_checked", side_effect=run):
                release_helper._audit_zip(archive, "vst3", "", cwd=root, require_production=False)

    def test_zip_audit_rejects_clap_code_resources_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "wave-v0.2.0-macos.clap.zip"
            archive.write_bytes(b"placeholder")

            def run(args, *, cwd, capture_output=False):
                if args[0] == "/usr/bin/ditto":
                    extracted = Path(args[-1])
                    binary = extracted / "wave.clap" / "Contents" / "MacOS" / "wave"
                    binary.parent.mkdir(parents=True)
                    (binary.parent.parent / "Info.plist").write_text("plist", encoding="utf-8")
                    (binary.parent.parent / "PkgInfo").write_text("BNDL????", encoding="utf-8")
                    (binary.parent.parent / "CodeResources").mkdir()
                    binary.write_bytes(b"arm64")
                    os.chmod(binary, 0o755)
                return subprocess.CompletedProcess(args, 0, "", "")

            with self.assertRaisesRegex(ValueError, r"clap ZIP Contents/CodeResources must be a regular file"), mock.patch.object(release_helper.platform, "system", return_value="Darwin"), mock.patch.object(release_helper, "_run_checked", side_effect=run):
                release_helper._audit_zip(archive, "clap", "TEAM123456", cwd=root)

    def test_zip_audit_rejects_wrong_team_before_stapler(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "wave-v0.2.0-macos.clap.zip"; archive.write_bytes(b"placeholder")

            def run(args, *, cwd, capture_output=False):
                if args[0] == "/usr/bin/ditto":
                    extracted = Path(args[-1]); binary = extracted / "wave.clap" / "Contents" / "MacOS" / "wave"; binary.parent.mkdir(parents=True)
                    (binary.parent.parent / "Info.plist").write_text("plist", encoding="utf-8"); (binary.parent.parent / "PkgInfo").write_text("BNDL????", encoding="utf-8"); binary.write_bytes(b"arm64"); os.chmod(binary, 0o755)
                if args[0] == "/usr/bin/plutil" and "CFBundleIdentifier" in args: return subprocess.CompletedProcess(args, 0, "com.portalsurfer.wave.clap\n", "")
                if args[0] == "/usr/bin/plutil" and "CFBundlePackageType" in args: return subprocess.CompletedProcess(args, 0, "BNDL\n", "")
                if args[0] == "codesign" and args[1] == "-dv": return subprocess.CompletedProcess(args, 0, "", "Authority=Developer ID Application: PORTALSURFER\nTeamIdentifier=OTHERTEAM\n")
                return subprocess.CompletedProcess(args, 0, "", "")

            with self.assertRaisesRegex(ValueError, "team does not match"), mock.patch.object(release_helper.platform, "system", return_value="Darwin"), mock.patch.object(release_helper, "_run_checked", side_effect=run):
                release_helper._audit_zip(archive, "clap", "TEAM123456", cwd=root)

    def test_source_gate_fetches_origin_before_comparing_refs(self):
        sha = "a" * 40
        manifest = {"source": {"git_sha": sha}}
        calls = []

        def run(args, *, cwd, capture_output=False):
            calls.append(tuple(args))
            output = {
                ("git", "symbolic-ref", "--quiet", "--short", "HEAD"): "main\n",
                ("git", "status", "--porcelain", "--untracked-files=all"): "",
                ("git", "rev-parse", "HEAD"): f"{sha}\n",
                ("git", "rev-parse", "refs/remotes/origin/main"): f"{sha}\n",
            }.get(tuple(args), "")
            return subprocess.CompletedProcess(args, 0, output, "")

        with mock.patch.object(release_helper, "_run_checked", side_effect=run):
            release_helper._validate_canonical_source(manifest, Path("/repo"))
        self.assertEqual(calls[2], ("git", "fetch", "origin", "main", "--quiet"))

    def test_source_gate_reports_exact_dirty_status_entries(self):
        sha = "a" * 40
        manifest = {"source": {"git_sha": sha}}
        status = " M scripts/release.sh\n?? target/release-note.txt\n"

        def run(args, *, cwd, capture_output=False):
            output = status if args[:2] == ("git", "status") else "main\n"
            return subprocess.CompletedProcess(args, 0, output, "")

        with mock.patch.object(release_helper, "_run_checked", side_effect=run):
            with self.assertRaisesRegex(
                ValueError,
                r"production release source must be clean; git status entries:  M scripts/release\.sh \| \?\? target/release-note\.txt",
            ):
                release_helper._validate_canonical_source(manifest, Path("/repo"))

    def test_publish_rejects_more_than_two_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            screenshot = root / "shot.png"; screenshot.write_bytes(png())
            clap = root / "a.zip"; clap.write_bytes(b"a")
            vst3 = root / "b.zip"; vst3.write_bytes(b"b")
            changelog = root / "CHANGELOG.md"; changelog.write_text("release\n", encoding="utf-8")
            manifest = release_helper.build_manifest(version="0.2.0", build_id="wave-v0.2.0-test", channel="stable", released_at="2026-07-28T00:00:00Z", git_sha="a" * 40, clap=clap, vst3=vst3, screenshot=screenshot, changelog=changelog, distribution="production", signing_identity_class="Developer ID Application", notarized=True, stapled=True, signing_team_id="TEAM123456", notary_submissions={"clap": "12345678-1234-4123-8123-123456789abc", "vst3": "abcdefab-cdef-4abc-8def-abcdefabcdef"})
            manifest["artifacts"].append(dict(manifest["artifacts"][0]))
            with self.assertRaisesRegex(ValueError, "exactly CLAP and VST3"):
                release_helper.validate_publish_manifest(manifest, root)

    def test_production_manifest_rejects_ad_hoc_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            screenshot = root / "shot.png"; screenshot.write_bytes(png())
            clap = root / "a.zip"; clap.write_bytes(b"a")
            vst3 = root / "b.zip"; vst3.write_bytes(b"b")
            changelog = root / "CHANGELOG.md"; changelog.write_text("release\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "production manifests"):
                release_helper.build_manifest(version="0.2.0", build_id="wave-v0.2.0-test", channel="stable", released_at="2026-07-28T00:00:00Z", git_sha="a" * 40, clap=clap, vst3=vst3, screenshot=screenshot, changelog=changelog, distribution="production")


if __name__ == "__main__":
    unittest.main()
