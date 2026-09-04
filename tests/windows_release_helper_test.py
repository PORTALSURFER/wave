import hashlib
import json
import tempfile
import unittest
import zipfile
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))
import windows_release_helper as helper


SOURCE_SHA = "a" * 40
PACKAGE_VERSION = "0.1.21"
PUBLICATION_VERSION = "0.1.21-nightly.42"
BUILD_ID = f"wave-v{PUBLICATION_VERSION}-{SOURCE_SHA[:12]}"
RELEASED_AT = "2026-09-04T18:00:00Z"
BUILD_ENVIRONMENT = {
    "runner": {"image": "windows-2022", "image_os": "win22", "image_version": "20260901.1.0"},
    "rust": {"toolchain": "1.97.1", "target": "x86_64-pc-windows-msvc", "rustc_version": "rustc 1.97.1 (000000000 2026-01-01)"},
    "python": {"implementation": "CPython", "version": "3.13.7"},
}


def pe_bytes(*, certificate=False):
    pe_offset = 0x80
    optional_size = 0xF0
    data = bytearray(pe_offset + 24 + optional_size + 32)
    data[:2] = b"MZ"
    data[0x3C : 0x40] = pe_offset.to_bytes(4, "little")
    data[pe_offset : pe_offset + 4] = b"PE\0\0"
    data[pe_offset + 4 : pe_offset + 6] = helper.IMAGE_FILE_MACHINE_AMD64.to_bytes(2, "little")
    data[pe_offset + 20 : pe_offset + 22] = optional_size.to_bytes(2, "little")
    optional = pe_offset + 24
    data[optional : optional + 2] = helper.PE32_PLUS_MAGIC.to_bytes(2, "little")
    data[optional + 108 : optional + 112] = (16).to_bytes(4, "little")
    if certificate:
        directory = optional + helper.PE_DATA_DIRECTORY_OFFSET + helper.IMAGE_DIRECTORY_ENTRY_SECURITY * 8
        data[directory : directory + 4] = (512).to_bytes(4, "little")
        data[directory + 4 : directory + 8] = (32).to_bytes(4, "little")
    return bytes(data)


class WindowsReleaseHelperTests(unittest.TestCase):
    def _lockfile(self, root):
        lockfile = root / "Cargo.lock"
        lockfile.write_text(
            f'''version = 3

[[package]]
name = "toybox"
version = "0.1.0"
source = "git+https://github.com/PORTALSURFER/toybox.git?rev={"a" * 40}#{"a" * 40}"

[[package]]
name = "radiant"
version = "0.1.0"
source = "git+https://github.com/PORTALSURFER/radiant.git?rev={"b" * 40}#{"b" * 40}"
''',
            encoding="utf-8",
        )
        return lockfile

    def _dependencies(self, lockfile):
        return helper.dependency_revisions(lockfile, vst3_sdk_revision="c" * 40)

    def _package(self, root, *, certificate=False, extra_member=False):
        binary = root / "dist" / helper.bundle_name(PACKAGE_VERSION) / "Contents" / "x86_64-win" / helper.bundle_name(PACKAGE_VERSION)
        binary.parent.mkdir(parents=True)
        binary.write_bytes(pe_bytes(certificate=certificate))
        lockfile = self._lockfile(root)
        output = root / "release"
        if extra_member:
            output.mkdir()
            archive = output / helper.archive_name(PUBLICATION_VERSION)
            with zipfile.ZipFile(archive, "w") as archive_file:
                archive_file.writestr(helper.bundle_member_name(PACKAGE_VERSION), pe_bytes())
                archive_file.writestr("unexpected", b"x")
            return binary, lockfile, output
        manifest = helper.package_windows_vst3(
            binary=binary,
            output_dir=output,
            package_version=PACKAGE_VERSION,
            publication_version=PUBLICATION_VERSION,
            channel="nightly",
            build_id=BUILD_ID,
            released_at=RELEASED_AT,
            source_sha=SOURCE_SHA,
            dependencies=self._dependencies(lockfile),
            build_environment=BUILD_ENVIRONMENT,
        )
        return binary, lockfile, output, manifest

    def test_product_specific_bundle_archive_and_sidecar(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self._package(Path(directory))
            _, lockfile, output, manifest = result
            archive = output / helper.archive_name(PUBLICATION_VERSION)
            self.assertEqual(helper.bundle_name(PACKAGE_VERSION), "WAVE-v0.1.21.vst3")
            self.assertEqual(helper.bundle_member_name(PACKAGE_VERSION), "WAVE-v0.1.21.vst3/Contents/x86_64-win/WAVE-v0.1.21.vst3")
            self.assertEqual(archive.name, "wave-v0.1.21-nightly.42-windows-x86_64-unsigned.vst3.zip")
            self.assertEqual(manifest["signing_status"], "unsigned")
            self.assertIsNone(manifest["signing_certificate"])
            helper.validate_manifest(json.loads((output / "windows-artifact-manifest.json").read_text()), output, cargo_lock=lockfile, vst3_sdk_revision="c" * 40)
            with zipfile.ZipFile(archive) as zip_file:
                self.assertEqual(zip_file.namelist(), [helper.bundle_member_name(PACKAGE_VERSION)])
                self.assertEqual(zip_file.getinfo(zip_file.namelist()[0]).date_time, (1980, 1, 1, 0, 0, 0))

    def test_dependency_revisions_are_pinned_to_lockfile_and_sdk(self):
        with tempfile.TemporaryDirectory() as directory:
            lockfile = self._lockfile(Path(directory))
            dependencies = helper.dependency_revisions(lockfile, vst3_sdk_revision="c" * 40)
            self.assertEqual(dependencies["toybox"]["revision"], "a" * 40)
            self.assertEqual(dependencies["radiant"]["revision"], "b" * 40)
            self.assertEqual(dependencies["vst3sdk"]["revision"], "c" * 40)

    def test_pe32_plus_and_unsigned_authenticode_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "binary"
            binary.write_bytes(pe_bytes())
            self.assertEqual(helper.validate_pe_binary(binary)[1], len(pe_bytes()))
            binary.write_bytes(pe_bytes(certificate=True))
            with self.assertRaisesRegex(ValueError, "Authenticode"):
                helper.validate_pe_binary(binary)
            binary.write_bytes(pe_bytes())
            with self.assertRaisesRegex(ValueError, "x86_64 PE32"):
                corrupted = bytearray(binary.read_bytes())
                corrupted[0x84 : 0x86] = (0x14C).to_bytes(2, "little")
                binary.write_bytes(corrupted)
                helper.validate_pe_binary(binary)

    def test_archive_topology_rejects_extra_members(self):
        with tempfile.TemporaryDirectory() as directory:
            _, _, output = self._package(Path(directory), extra_member=True)
            with self.assertRaisesRegex(ValueError, "exactly one file"):
                helper.validate_archive(output / helper.archive_name(PUBLICATION_VERSION), package_version=PACKAGE_VERSION, publication_version=PUBLICATION_VERSION)

    def test_source_path_is_exact_and_outputs_are_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "dist" / "wrong.vst3"
            binary.parent.mkdir()
            binary.write_bytes(pe_bytes())
            with self.assertRaisesRegex(ValueError, "build output must be under"):
                helper.package_windows_vst3(binary=binary, output_dir=root / "release", package_version=PACKAGE_VERSION, publication_version=PUBLICATION_VERSION, channel="nightly", build_id=BUILD_ID, released_at=RELEASED_AT, source_sha=SOURCE_SHA, dependencies={}, build_environment=BUILD_ENVIRONMENT)


if __name__ == "__main__":
    unittest.main()
