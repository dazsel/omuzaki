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
        self.assertEqual(len(names), 28)  # design §3.2: 25 C API functions and the shim's 3
        self.assertEqual(len(set(names)), len(names))
        self.assertEqual(names[-3:], ["omz_litert_abi", "omz_litert_xnnpack_create", "omz_litert_xnnpack_delete"])

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
    def test_archives_are_reproducible_and_unpack(self):
        files = {"lib/x.so": b"\x7fELF", "LICENSE": b"text"}
        for kind in ("tar.gz", "zip"):
            first = build.archive_bytes("stem", files, kind)
            self.assertEqual(first, build.archive_bytes("stem", dict(reversed(files.items())), kind), kind)
            with tempfile.TemporaryDirectory() as tmp:
                dist = Path(tmp) / "dist"
                dist.mkdir()
                (dist / f"stem.{kind}").write_bytes(first)
                (dist / "SHA256SUMS").write_text(f"{build.sha256(dist / f'stem.{kind}')}  stem.{kind}\n")
                folder = build.unpack(dist, Path(tmp) / "out")
                self.assertEqual((folder / "lib/x.so").read_bytes(), b"\x7fELF", kind)

    def test_a_changed_archive_fails_its_checksum(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = Path(tmp)
            (dist / "stem.zip").write_bytes(b"changed")
            (dist / "SHA256SUMS").write_text(f"{'0' * 64}  stem.zip\n")
            with self.assertRaisesRegex(build.Mismatch, "SHA-256 of stem.zip"):
                build.unpack(dist, dist / "out")


if __name__ == "__main__":
    unittest.main()
