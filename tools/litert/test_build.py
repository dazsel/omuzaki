#!/usr/bin/env python3
"""Tests for tools/litert/build.py and its pins.

The workflow's build job runs them first: `python3 tools/litert/test_build.py`.
"""

import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build  # noqa: E402

DEPS = build.HERE.parent.parent / "docs/licensing/deps"
LICENCES = tomllib.loads((build.HERE / "licences.toml").read_text(encoding="utf-8"))["repo"]


class Pins(unittest.TestCase):
    def test_shim_abi_matches_the_pin(self):
        self.assertEqual(build.shim_abi(), build.PINS["shim"]["abi"])

    def test_symbols_are_the_design_list(self):
        names = build.symbols()
        self.assertEqual(len(names), 28)  # design §3.2: 24 C API functions and the shim's 4
        self.assertEqual(len(set(names)), len(names))
        shim = ["omz_litert_abi", "omz_litert_xnnpack_create", "omz_litert_xnnpack_delete", "omz_litert_capture_errors"]
        self.assertEqual(names[-4:], shim)
        # The reporter takes a va_list, so only the shim calls it (design §3.3).
        self.assertNotIn("TfLiteInterpreterOptionsSetErrorReporter", names)

    def test_every_platform_names_its_tools_and_library(self):
        for name, plat in build.PINS["platform"].items():
            for key in ("bazelisk_asset", "bazelisk_sha256", "bazel_sha256", "flags", "library", "archive"):
                self.assertIn(key, plat, name)
            self.assertIn(plat["archive"], ("tar.gz", "zip"), name)

    def test_every_licence_entry_has_a_register_row_and_files(self):
        for repo, entry in LICENCES.items():
            self.assertTrue((DEPS / f"{entry['row']}.toml").is_file(), repo)
            self.assertTrue(entry["files"], repo)
            self.assertTrue(entry["spdx"], repo)


class ExportLists(unittest.TestCase):
    def test_version_script_exports_only_the_names(self):
        script = build.version_script(["A", "B"])
        self.assertIn("global:\n    A;\n    B;\n  local:\n    *;", script)

    def test_apple_list_prefixes_an_underscore(self):
        self.assertEqual(build.exported_symbols_list(["A", "B"]), "_A\n_B\n")


class ReadBack(unittest.TestCase):
    def test_max_version_compares_numerically(self):
        text = "Name: GLIBC_2.2.5\nName: GLIBC_2.17\nName: GLIBC_2.9\nName: GLIBCXX_3.4.30"
        self.assertEqual(build.max_version(text, "GLIBC_"), "2.17")
        self.assertEqual(build.max_version("nothing", "GLIBC_"), "")

    def test_delegated_reads_the_plan_line(self):
        self.assertEqual(build.delegated("delegated 210 of 210 in 1 partitions\n"), (210, 210, 1))
        with self.assertRaises(build.Mismatch):
            build.delegated("")


try:
    import numpy
except ImportError:  # the build job has no numpy; the check job runs these too, with it
    numpy = None


@unittest.skipIf(numpy is None, "numpy is installed in the check job")
class Parity(unittest.TestCase):
    def test_the_largest_difference_is_returned(self):
        ours, reference = numpy.array([1.0, 2.0], numpy.float32), numpy.array([1.0, 2.5], numpy.float32)
        self.assertEqual(build.max_abs_diff(ours, reference, "out"), 0.5)

    def test_a_non_finite_value_on_either_side_fails(self):
        finite = numpy.zeros(2, numpy.float32)
        for bad in (numpy.nan, numpy.inf):
            broken = numpy.array([0.0, bad], numpy.float32)
            with self.assertRaisesRegex(build.Mismatch, "non-finite"):
                build.max_abs_diff(broken, finite, "out")
            with self.assertRaisesRegex(build.Mismatch, "non-finite"):
                build.max_abs_diff(finite, broken, "out")


class Notices(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        for repo in ("litert", "XNNPACK"):
            (self.root / repo).mkdir()
            (self.root / repo / "LICENSE").write_text(f"{repo} licence text")
        self.roots = {repo: self.root / repo for repo in ("litert", "XNNPACK", "unknown")}

    def tearDown(self):
        self.tmp.cleanup()

    def test_repositories_keeps_those_with_sources(self):
        labels = ["//tflite/c:c_api.cc (null)", "@XNNPACK//src:init.c (null)", "@@cpuinfo//:cpuinfo (abc)",
                  "@bazel_skylib//lib:paths.bzl (null)", "@flatbuffers//:include/flatbuffers/base.h (null)"]
        self.assertEqual(build.repositories(labels), {"litert", "XNNPACK", "flatbuffers"})

    def test_notices_list_each_repository_with_its_licence(self):
        text, files = build.notices({"litert", "XNNPACK"}, LICENCES, self.roots, DEPS)
        self.assertIn("## XNNPACK", text)
        self.assertIn("XNNPACK licence text", text)
        self.assertEqual(sorted(files), ["XNNPACK-LICENSE.txt", "litert-LICENSE.txt"])

    def test_licence_files_in_subfolders_are_copied_flat(self):
        (self.root / "XNNPACK" / "LICENSES").mkdir()
        (self.root / "XNNPACK" / "LICENSES" / "MIT.txt").write_text("text")
        entry = dict(LICENCES["XNNPACK"], files=["LICENSES/MIT.txt"])
        _, files = build.notices({"XNNPACK"}, {"XNNPACK": entry}, self.roots, DEPS)
        self.assertEqual(list(files), ["XNNPACK-LICENSES-MIT.txt.txt"])

    def test_a_repository_without_an_entry_fails(self):
        with self.assertRaisesRegex(build.Mismatch, "unknown: linked, but has no entry"):
            build.notices({"litert", "unknown"}, LICENCES, self.roots, DEPS)

    def test_a_missing_licence_file_fails(self):
        (self.root / "XNNPACK" / "LICENSE").unlink()
        with self.assertRaisesRegex(build.Mismatch, "XNNPACK: licence file LICENSE is missing"):
            build.notices({"XNNPACK"}, LICENCES, self.roots, DEPS)

    def test_a_missing_register_row_fails(self):
        with self.assertRaisesRegex(build.Mismatch, "docs/licensing/deps/litert-xnnpack.toml does not exist"):
            build.notices({"XNNPACK"}, LICENCES, self.roots, self.root)


class Archive(unittest.TestCase):
    def test_archives_are_reproducible_and_unpack_against_the_build_digest(self):
        files = {"lib/x.so": b"\x7fELF", "LICENSE": b"text"}
        for kind in ("tar.gz", "zip"):
            first = build.archive_bytes("stem", files, kind)
            self.assertEqual(first, build.archive_bytes("stem", dict(reversed(files.items())), kind), kind)
            with tempfile.TemporaryDirectory() as tmp:
                asset = Path(tmp) / f"stem.{kind}"
                asset.write_bytes(first)
                folder = build.unpack(asset, build.sha256(asset), Path(tmp) / "out")
                self.assertEqual((folder / "lib/x.so").read_bytes(), b"\x7fELF", kind)

    def test_unpack_refuses_a_wrong_or_missing_digest(self):
        with tempfile.TemporaryDirectory() as tmp:
            asset = Path(tmp) / "stem.zip"
            asset.write_bytes(build.archive_bytes("stem", {"a": b"a"}, "zip"))
            with self.assertRaisesRegex(build.Mismatch, "SHA-256 of stem.zip"):
                build.unpack(asset, "0" * 64, Path(tmp) / "out")
            with self.assertRaisesRegex(build.Mismatch, "no valid SHA-256"):
                build.unpack(asset, "", Path(tmp) / "out")

    def test_archive_in_wants_the_platform_archive_and_its_sums_line_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = Path(tmp)
            asset = dist / f"{build.stem('win-x64')}.zip"
            asset.write_bytes(b"archive")
            (dist / "SHA256SUMS").write_text(f"{build.sha256(asset)}  {asset.name}\n")
            self.assertEqual(build.archive_in(dist, "win-x64"), asset)
            with self.assertRaisesRegex(build.Mismatch, "files in"):
                build.archive_in(dist, "linux-x64")
            (dist / "SHA256SUMS").write_text(f"{'0' * 64}  {asset.name}\n")
            with self.assertRaisesRegex(build.Mismatch, "SHA256SUMS of"):
                build.archive_in(dist, "win-x64")


class CacheKey(unittest.TestCase):
    def write(self, folder: Path, key: str, hit: str):
        (folder / "key").write_text(key + "\n")
        (folder / "hit").write_text(hit + "\n")

    def test_the_build_key_reads_back(self):
        key = f"litert-v1-win-x64-win22-20261005.1-{'a' * 64}"
        with tempfile.TemporaryDirectory() as tmp:
            self.write(Path(tmp), key, "false")
            self.assertEqual(build.cache_key(Path(tmp), "win-x64"), (key, False))

    def test_another_platform_or_a_bad_flag_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.write(Path(tmp), f"litert-v1-linux-x64-ubuntu24-1-{'a' * 64}", "false")
            with self.assertRaisesRegex(build.Mismatch, "not a litert key for win-x64"):
                build.cache_key(Path(tmp), "win-x64")
            self.write(Path(tmp), f"litert-v1-win-x64-win22-1-{'a' * 64}", "")
            with self.assertRaisesRegex(build.Mismatch, "neither true nor false"):
                build.cache_key(Path(tmp), "win-x64")


class Digests(unittest.TestCase):
    def write(self, folder: Path, digests: dict):
        for plat, digest in digests.items():
            (folder / f"{plat}.sha256").write_text(digest + "\n")

    def test_three_valid_digests_merge(self):
        digests = {plat: f"{index:064x}" for index, plat in enumerate(build.PINS["platform"])}
        with tempfile.TemporaryDirectory() as tmp:
            self.write(Path(tmp), digests)
            self.assertEqual(build.merge_digests(Path(tmp)), digests)

    def test_a_missing_or_malformed_digest_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.write(Path(tmp), {"linux-x64": "a" * 64, "win-x64": "b" * 64})
            with self.assertRaisesRegex(build.Mismatch, "digest files"):
                build.merge_digests(Path(tmp))
            self.write(Path(tmp), {"macos-arm64": "C" * 64})
            with self.assertRaisesRegex(build.Mismatch, "not a SHA-256"):
                build.merge_digests(Path(tmp))


if __name__ == "__main__":
    unittest.main()
