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


class BuildInfo(unittest.TestCase):
    TOOLCHAINS = {"linux-x64": {"compiler": "clang version 18.1.8", "glibc": "2.17"},
                  "win-x64": {"msvc_toolset": "14.44.35207"},
                  "macos-arm64": {"xcode": "16.4", "macos_minimum": "13.0"}}

    def info(self, plat: str) -> dict:
        return build.buildinfo(plat, dict(self.TOOLCHAINS[plat]))

    def test_a_pinned_build_has_no_problems(self):
        for plat in build.PINS["platform"]:
            self.assertEqual(build.buildinfo_problems(self.info(plat), plat), [], plat)

    def test_runner_specific_flags_stay_out_of_the_record(self):
        self.assertNotIn("python", " ".join(self.info("win-x64")["flags"]))

    def assert_one_problem(self, plat: str, change, field: str):
        info = self.info(plat)
        change(info)
        problems = build.buildinfo_problems(info, plat)
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(problems[0].startswith(field), problems)

    def test_each_field_mismatch_is_reported(self):
        cases = [
            ("litert", lambda i: i["litert"].update(commit="0" * 40)),
            ("platform", lambda i: i.update(platform="linux-x64")),
            ("omz_litert_abi", lambda i: i.update(omz_litert_abi=2)),
            ("library", lambda i: i.update(library="other.dll")),
            ("bazel", lambda i: i.update(bazel="7.6.0")),
            ("flags", lambda i: i.update(flags=[*i["flags"], "--copt=-O0"])),
            ("toolchain.msvc_toolset", lambda i: i["toolchain"].update(msvc_toolset="14.43.0")),
            ("toolchain:", lambda i: i.update(toolchain={})),
        ]
        for field, change in cases:
            with self.subTest(field=field):
                self.assert_one_problem("win-x64", change, field)

    def test_each_toolchain_value_is_checked(self):
        cases = [
            ("linux-x64", "toolchain.compiler", lambda i: i["toolchain"].update(compiler="clang version 19.1.0")),
            ("linux-x64", "toolchain.glibc", lambda i: i["toolchain"].update(glibc="2.28")),
            ("linux-x64", "toolchain.glibc", lambda i: i["toolchain"].update(glibc="")),
            ("macos-arm64", "toolchain.xcode", lambda i: i["toolchain"].update(xcode="16.2")),
            ("macos-arm64", "toolchain.macos_minimum", lambda i: i["toolchain"].update(macos_minimum="11.0")),
        ]
        for plat, field, change in cases:
            with self.subTest(plat=plat, field=field):
                self.assert_one_problem(plat, change, field)


class ExportLists(unittest.TestCase):
    def test_version_script_exports_only_the_names(self):
        script = build.version_script(["A", "B"])
        self.assertIn("global:\n    A;\n    B;\n  local:\n    *;", script)

    def test_apple_list_prefixes_an_underscore(self):
        self.assertEqual(build.exported_symbols_list(["A", "B"]), "_A\n_B\n")


def minimal_pe(names: list[str], magic: int = 0x20B) -> bytes:
    """A PE image with one section holding an export directory that names `names` (enough for pe_exports)."""
    pe, optional_size, section_rva, raw = 0x40, 240, 0x1000, 0x200
    strings, name_rvas = b"", []
    directory_size, pointers = 40, 4 * len(names)
    for name in names:
        name_rvas.append(section_rva + directory_size + pointers + len(strings))
        strings += name.encode() + b"\0"
    body = bytearray(directory_size)
    body[24:28] = len(names).to_bytes(4, "little")
    body[32:36] = (section_rva + directory_size).to_bytes(4, "little")
    body += b"".join(rva.to_bytes(4, "little") for rva in name_rvas) + strings
    image = bytearray(raw) + body
    image[0:2], image[0x3C:0x40] = b"MZ", pe.to_bytes(4, "little")
    image[pe:pe + 4] = b"PE\0\0"
    image[pe + 6:pe + 8] = (1).to_bytes(2, "little")
    image[pe + 20:pe + 22] = optional_size.to_bytes(2, "little")
    optional = pe + 24
    image[optional:optional + 2] = magic.to_bytes(2, "little")
    image[optional + 112:optional + 116] = section_rva.to_bytes(4, "little")
    section = optional + optional_size
    image[section + 8:section + 12] = len(body).to_bytes(4, "little")
    image[section + 12:section + 16] = section_rva.to_bytes(4, "little")
    image[section + 16:section + 20] = len(body).to_bytes(4, "little")
    image[section + 20:section + 24] = raw.to_bytes(4, "little")
    return bytes(image)


class Exports(unittest.TestCase):
    def test_exactly_the_list_passes(self):
        build.check_exports(set(build.symbols()), build.symbols())

    def test_an_extra_symbol_fails(self):
        with self.assertRaisesRegex(build.Mismatch, r"extra \['TfLiteTensorData'\], missing \[\]"):
            build.check_exports({*build.symbols(), "TfLiteTensorData"}, build.symbols())

    def test_a_missing_symbol_fails(self):
        with self.assertRaisesRegex(build.Mismatch, r"extra \[\], missing \['omz_litert_capture_errors'\]"):
            build.check_exports(set(build.symbols()) - {"omz_litert_capture_errors"}, build.symbols())


class WindowsExports(unittest.TestCase):
    def test_the_def_file_lists_every_name(self):
        self.assertEqual(build.module_definition(["A", "B"]), "EXPORTS\n    A\n    B\n")

    def test_exported_names_read_back(self):
        names = build.symbols()
        self.assertEqual(build.pe_exports(minimal_pe(names)), names)

    def test_other_files_are_refused(self):
        with self.assertRaisesRegex(build.Mismatch, "not a PE32"):
            build.pe_exports(minimal_pe(["A"], magic=0x10B))
        with self.assertRaisesRegex(build.Mismatch, "not a PE file"):
            build.pe_exports(b"\x7fELF" + bytes(0x100))


class ReadBack(unittest.TestCase):
    def test_max_version_compares_numerically(self):
        text = "Name: GLIBC_2.2.5\nName: GLIBC_2.17\nName: GLIBC_2.9\nName: GLIBCXX_3.4.30"
        self.assertEqual(build.max_version(text, "GLIBC_"), "2.17")
        self.assertEqual(build.max_version("nothing", "GLIBC_"), "")

    def test_delegated_reads_the_plan_line(self):
        self.assertEqual(build.delegated("delegated 210 of 210 in 1 partitions\n"), (210, 210, 1, []))
        with self.assertRaises(build.Mismatch):
            build.delegated("")

    def test_delegated_lists_the_ops_left_out(self):
        printed = "delegated 180 of 182 in 1 partitions\nundelegated 22\nundelegated custom:Foo\n"
        self.assertEqual(build.delegated(printed), (180, 182, 1, ["22", "custom:Foo"]))
        with self.assertRaisesRegex(build.Mismatch, "listed 1 undelegated nodes for 2 left out"):
            build.delegated("delegated 180 of 182 in 1 partitions\nundelegated 22\n")


class Delegation(unittest.TestCase):
    def test_the_measured_counts_match_the_pins(self):
        self.assertEqual(build.delegation_problem("face_detector", 164, 164, 1, []), "")
        self.assertEqual(build.delegation_problem("face_landmarks_detector", 471, 471, 1, []), "")
        self.assertEqual(build.delegation_problem("face_blendshapes", 181, 182, 1, ["45"]), "")

    def test_a_change_in_either_direction_fails(self):
        cases = [("face_detector", 163, 164, 1, ["3"]), ("face_detector", 164, 164, 2, []),
                 ("face_blendshapes", 182, 182, 1, []), ("face_blendshapes", 180, 182, 1, ["45", "3"]),
                 ("face_blendshapes", 181, 183, 1, ["45"]), ("face_blendshapes", 181, 182, 2, ["45"])]
        for graph, taken, total, partitions, left in cases:
            with self.subTest(graph=graph, taken=taken, total=total, partitions=partitions):
                self.assertIn("pinned", build.delegation_problem(graph, taken, total, partitions, left))

    def test_another_op_left_out_fails_at_the_same_count_and_names_both(self):
        why = build.delegation_problem("face_blendshapes", 181, 182, 1, ["22"])
        self.assertIn("leaving 22 on builtin kernels", why)
        self.assertIn("leaving STRIDED_SLICE (45)", why)

    def test_every_face_graph_has_a_pin(self):
        self.assertEqual(sorted(build.PINS["delegation"]), sorted(build.FACE_GRAPHS))


try:
    import numpy
except ImportError:  # the build job has no numpy; the check job runs these too, with it
    numpy = None


@unittest.skipIf(numpy is None, "numpy is installed in the check job")
class Parity(unittest.TestCase):
    def test_the_largest_difference_is_returned(self):
        ours, reference = numpy.array([1.0, 2.0], numpy.float32), numpy.array([1.0, 2.5], numpy.float32)
        self.assertEqual(build.max_abs_diff(ours, reference, "out"), (0.5, ""))

    def test_nan_plus_finite_keeps_the_finite_difference_and_reports_the_nan(self):
        ours, reference = numpy.array([numpy.nan, 0.5], numpy.float32), numpy.zeros(2, numpy.float32)
        worst, problem = build.max_abs_diff(ours, reference, "out")
        self.assertEqual(worst, 0.5)
        self.assertEqual(problem, "out: 1 non-finite value(s), the first at index 0")

    def test_a_non_finite_value_on_either_side_is_reported_with_count_and_first_index(self):
        finite = numpy.zeros(3, numpy.float32)
        for bad in (numpy.nan, numpy.inf):
            broken = numpy.array([0.0, bad, bad], numpy.float32)
            for ours, reference in ((broken, finite), (finite, broken)):
                self.assertEqual(build.max_abs_diff(ours, reference, "out"),
                                 (0.0, "out: 2 non-finite value(s), the first at index 1"))

    def test_nan_plus_finite_over_the_gate_fails_on_both_counts(self):
        wheel = [numpy.zeros(2, numpy.float32)]
        ours = numpy.array([numpy.nan, 0.5], numpy.float32).tobytes()
        worst, problems = build.graph_parity("g", ours, wheel)
        self.assertEqual(worst, 0.5, "the tolerance gate sees the finite difference")
        self.assertEqual(problems, ["g output 0: 1 non-finite value(s), the first at index 0"])

    def test_a_non_finite_output_is_a_problem_and_the_other_outputs_are_still_measured(self):
        wheel = [numpy.zeros(2, numpy.float32), numpy.zeros(2, numpy.float32)]
        ours = numpy.array([0.0, numpy.nan, 0.25, 0.0], numpy.float32).tobytes()
        worst, problems = build.graph_parity("g", ours, wheel)
        self.assertEqual(worst, 0.25)
        self.assertEqual(len(problems), 1)
        self.assertRegex(problems[0], "g output 0: 1 non-finite")

    def test_finite_outputs_give_the_largest_difference_and_no_problem(self):
        wheel = [numpy.zeros(2, numpy.float32), numpy.zeros(1, numpy.float32)]
        ours = numpy.array([0.0, 0.5, 0.25], numpy.float32).tobytes()
        self.assertEqual(build.graph_parity("g", ours, wheel), (0.5, []))


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
