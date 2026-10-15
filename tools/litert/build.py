#!/usr/bin/env python3
"""Builds, packs and checks Omuzaki's LiteRT library; the steps of .github/workflows/runtime-litert.yml.

ADR 0061 decision 5 and docs/design/litert-runtime.md §8. Each subcommand is one workflow step, and each fails the job
when what it finds differs from tools/litert/pins.toml:

  setup    fetch LiteRT at the pinned commit, check the tag and toolchain pins, add the overlay, install Bazelisk
  build    build the library and the check's two programs; read the compiler, glibc floor or minimum OS and the
           exported symbols back from the library
  notices  list the external repositories the library's sources come from; licence files and THIRD_PARTY_NOTICES.md
  pack     the archive (lib/, LICENSE, notices, licences/, BUILDINFO.json) and its SHA256SUMS
  digest   hash the archive (built or restored from the cache) into <platform>.sha256 and the run summary
  collect  merge the three platforms' digests into the workflow's outputs, refusing anything but three valid digests
  check    verify an archive, load it with the smoke program and compare three face graphs with the official wheel
  verify   before the cache save: the archive against the build's digest, and the cache key the build used

`container` prints the image the Linux build runs in, for the workflow's `docker run`.

Standard library only (Python 3.11 or newer, for tomllib), so it runs before anything is installed.
"""

import argparse
import functools
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
PINS = tomllib.loads((HERE / "pins.toml").read_text(encoding="utf-8"))
OVERLAY = "omuzaki"  # the overlay package inside the LiteRT tree
BAZELISK_URL = "https://github.com/bazelbuild/bazelisk/releases/download/{version}/{asset}"
# What omz_litert_plan prints: the nodes the delegate took, the nodes in the graph, the partitions.
PLAN = re.compile(r"^delegated (\d+) of (\d+) in (\d+) partitions$", re.MULTILINE)
# The three face graphs the check runs, as tools/models/mediapipe-face.sh writes them.
FACE_GRAPHS = ("face_detector", "face_landmarks_detector", "face_blendshapes")
SOURCE_SUFFIXES = (".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".inc", ".S", ".s", ".asm")


class Mismatch(Exception):
    """Something the job found differs from a pin, or a gate failed; the message says what."""


def run(cmd, cwd=None, env=None, capture=True, stdin=None):
    """Runs `cmd`, echoing it, and returns its stdout; raises Mismatch with its stderr when it fails."""
    print("+", " ".join(str(c) for c in cmd), flush=True)
    done = subprocess.run([str(c) for c in cmd], cwd=cwd, env=env, input=stdin, text=True,
                          stdout=subprocess.PIPE if capture else None, stderr=subprocess.PIPE if capture else None)
    if done.returncode != 0:
        raise Mismatch(f"{cmd[0]} exited with {done.returncode}: {(done.stderr or '').strip()[-2000:]}")
    return done.stdout or ""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def expect(what: str, found, pinned):
    if found != pinned:
        raise Mismatch(f"{what}: found {found!r}, pinned {pinned!r} (tools/litert/pins.toml)")


def symbols() -> list[str]:
    lines = (HERE / "symbols.txt").read_text(encoding="utf-8").splitlines()
    return [line.strip() for line in lines if line.strip() and not line.startswith("#")]


def version_script(names: list[str]) -> str:
    """The GNU ld version script that exports exactly `names`."""
    exported = "".join(f"    {name};\n" for name in names)
    return f"VERS_1.0 {{\n  global:\n{exported}  local:\n    *;\n}};\n"


def exported_symbols_list(names: list[str]) -> str:
    """The Apple ld `-exported_symbols_list` file for `names` (C symbols take a leading underscore)."""
    return "".join(f"_{name}\n" for name in names)


def shim_abi() -> int:
    source = (HERE / "omz_litert_shim.c").read_text(encoding="utf-8")
    found = re.search(r"#define OMZ_LITERT_ABI (\d+)u", source)
    if not found:
        raise Mismatch("omz_litert_shim.c defines no OMZ_LITERT_ABI")
    return int(found.group(1))


def platform_pins(name: str) -> dict:
    if name not in PINS["platform"]:
        raise Mismatch(f"unknown platform {name!r}; pins.toml has {', '.join(PINS['platform'])}")
    return PINS["platform"][name]


def bazel_path(tools: Path) -> Path:
    return tools / ("bazel.exe" if os.name == "nt" else "bazel")


def bazel_env(plat: str) -> dict:
    """The environment Bazel runs in: Bazelisk checks the Bazel binary, Windows gets the pinned MSVC toolset."""
    env = dict(os.environ, BAZELISK_VERIFY_SHA256=platform_pins(plat)["bazel_sha256"])
    # Runner images point this at an Android NDK. LiteRT's WORKSPACE then instantiates an NDK repository, which fails
    # on Windows and is an image input outside the cache key elsewhere; nothing Android is built (LiteRT's own Windows
    # CI blanks it too).
    env.pop("ANDROID_NDK_HOME", None)
    if plat == "win-x64":
        env["BAZEL_VC"] = str(visual_studio() / "VC")
        env["BAZEL_VC_FULL_VERSION"] = platform_pins(plat)["msvc_toolset"]
        env.setdefault("BAZEL_SH", r"C:\Program Files\Git\bin\bash.exe")
    if plat == "macos-arm64":
        env["DEVELOPER_DIR"] = f"/Applications/Xcode_{platform_pins(plat)['xcode']}.app/Contents/Developer"
    return env


def bazel(args, litert: Path, tools: Path, plat: str, capture=True) -> str:
    # A short output root on Windows keeps Bazel's paths under MAX_PATH.
    startup = ["--output_user_root=C:/b"] if plat == "win-x64" else []
    return run([bazel_path(tools), *startup, *args], cwd=litert, env=bazel_env(plat), capture=capture)


def pinned_flags(plat: str) -> list[str]:
    """The Bazel flags pins.toml sets for `plat`, as BUILDINFO.json records them."""
    return [*PINS["bazel"]["flags"], *platform_pins(plat)["flags"]]


def build_flags(plat: str) -> list[str]:
    """`pinned_flags` plus what only this runner knows: on Windows, the path of the Python the job set up."""
    flags = pinned_flags(plat)
    if plat == "win-x64":
        # LiteRT's windows config names a fixed Python path; use the one this job set up.
        flags += [f"--action_env=PYTHON_BIN_PATH={sys.executable}", f"--python_path={sys.executable}"]
    return flags


@functools.cache
def visual_studio() -> Path:
    """The newest Visual Studio on the Windows image, from its own locator."""
    program_files = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)"))
    installer = program_files / "Microsoft Visual Studio/Installer"
    vswhere = installer / "vswhere.exe"
    return Path(run([vswhere, "-latest", "-products", "*", "-property", "installationPath"]).strip())


def check_toolchain(plat: str):
    """The toolchain pins Bazel itself doesn't check: the MSVC toolset is installed, the Xcode is the pinned one."""
    pins = platform_pins(plat)
    if plat == "win-x64":
        toolsets = sorted(p.name for p in (visual_studio() / "VC/Tools/MSVC").iterdir())
        if pins["msvc_toolset"] not in toolsets:
            raise Mismatch(f"MSVC toolset {pins['msvc_toolset']} is not installed; the image has {', '.join(toolsets)}")
    if plat == "macos-arm64":
        first = run(["xcodebuild", "-version"], env=bazel_env(plat)).splitlines()[0]
        expect("Xcode", first, f"Xcode {pins['xcode']}")


def cmd_setup(args):
    pins, litert, tools = PINS["litert"], Path(args.litert), Path(args.tools)
    expect("OMZ_LITERT_ABI in omz_litert_shim.c", shim_abi(), PINS["shim"]["abi"])
    check_toolchain(args.platform)
    litert.mkdir(parents=True, exist_ok=True)
    run(["git", "init", "-q"], cwd=litert)
    run(["git", "fetch", "-q", "--depth", "1", pins["repository"], pins["commit"]], cwd=litert)
    run(["git", "checkout", "-q", "FETCH_HEAD"], cwd=litert)
    # The tag must still name the pinned commit: `^{}` is the commit an annotated tag points at.
    listed = run(["git", "ls-remote", pins["repository"], f"refs/tags/{pins['tag']}", f"refs/tags/{pins['tag']}^{{}}"])
    refs = {ref: commit for commit, ref in (line.split("\t") for line in listed.splitlines())}
    tag_commit = refs.get(f"refs/tags/{pins['tag']}^{{}}", refs.get(f"refs/tags/{pins['tag']}"))
    expect(f"commit of LiteRT tag {pins['tag']}", tag_commit, pins["commit"])
    expect("LiteRT's .bazelversion", (litert / ".bazelversion").read_text().strip(), PINS["bazel"]["version"])

    overlay = litert / OVERLAY
    overlay.mkdir(exist_ok=True)
    shutil.copy(HERE / "BUILD.omuzaki", overlay / "BUILD")
    for name in ("omz_litert_shim.c", "omz_litert_smoke.c", "omz_litert_plan.cc"):
        shutil.copy(HERE / name, overlay / name)
    (overlay / "version_script.lds").write_text(version_script(symbols()))
    (overlay / "exported_symbols.lds").write_text(exported_symbols_list(symbols()))

    plat = platform_pins(args.platform)
    tools.mkdir(parents=True, exist_ok=True)
    url = BAZELISK_URL.format(version=PINS["bazel"]["bazelisk"], asset=plat["bazelisk_asset"])
    with urllib.request.urlopen(url) as response:
        bazelisk_path = bazel_path(tools)
        bazelisk_path.write_bytes(response.read())
    expect("Bazelisk SHA-256", sha256(bazelisk_path), plat["bazelisk_sha256"])
    bazelisk_path.chmod(0o755)
    # `--version` is itself a startup option, so it goes without the Windows output root `bazel()` adds.
    reported = run([bazel_path(tools), "--version"], cwd=litert, env=bazel_env(args.platform)).strip()
    expect("Bazel", reported, f"bazel {PINS['bazel']['version']}")

    # LiteRT's configure.py asks its questions on stdin; these answers match LiteRT's own CI.
    env = dict(bazel_env(args.platform), PYTHON_BIN_PATH=sys.executable, TF_NEED_ROCM="0", TF_NEED_CUDA="0",
               TF_NEED_CLANG="0", TF_SET_ANDROID_WORKSPACE="0", CC_OPT_FLAGS="-Wno-sign-compare",
               PATH=f"{tools}{os.pathsep}{os.environ['PATH']}")
    env["PYTHON_LIB_PATH"] = run([sys.executable, "-c", "import site; print(site.getsitepackages()[0])"]).strip()
    run([sys.executable, "configure.py"], cwd=litert, env=env, stdin="\n" * 50)


def built_file(target: str, suffixes: tuple, args) -> Path:
    litert, tools = Path(args.litert), Path(args.tools)
    cquery = ["cquery", *build_flags(args.platform), "--output=files", target]
    files = [line for line in bazel(cquery, litert, tools, args.platform).splitlines() if line.endswith(suffixes)]
    if len(files) != 1:
        raise Mismatch(f"{target} built {files}, expected one file ending {suffixes}")
    root = bazel(["info", *build_flags(args.platform), "execution_root"], litert, tools, args.platform).strip()
    return Path(root) / files[0]


def max_version(text: str, prefix: str) -> str:
    """The newest `<prefix>x.y[.z]` named in `text`, e.g. the glibc floor from a library's version needs."""
    found = {tuple(int(part) for part in v.split(".")) for v in re.findall(rf"\b{prefix}(\d+(?:\.\d+)+)\b", text)}
    return ".".join(map(str, max(found))) if found else ""


def read_back(plat: str, library: Path, litert: Path, tools: Path) -> dict:
    """The toolchain facts read back from the built library, checked against the pins."""
    pins, names = platform_pins(plat), set(symbols())
    if plat == "linux-x64":
        # LLVM's tools from the hermetic toolchain the build used, so the container needs no binutils.
        llvm = Path(bazel(["info", "output_base"], litert, tools, plat).strip()) / "external/llvm18_linux_x86_64/bin"
        comment = run([llvm / "llvm-readelf", "-p", ".comment", library])
        if pins["compiler"] not in comment:
            raise Mismatch(f"the library's .comment does not name {pins['compiler']!r}: {comment.strip()}")
        glibc = max_version(run([llvm / "llvm-readelf", "-V", library]), "GLIBC_")
        if version_tuple(glibc) > version_tuple(pins["glibc_max"]):
            raise Mismatch(f"the library needs glibc {glibc}, newer than the pinned floor {pins['glibc_max']}")
        # Names carry their version node (`TfLiteVersion@@VERS_1.0`), and the node itself is listed as a symbol.
        listed = run([llvm / "llvm-nm", "-D", "--defined-only", "--format=just-symbols", library]).split()
        exported = {name.split("@")[0] for name in listed} - {"VERS_1.0"}
        expect("exported symbols", sorted(exported), sorted(names))
        return {"compiler": pins["compiler"], "glibc": glibc}
    if plat == "macos-arm64":
        loads = run(["otool", "-l", library])
        minos = re.search(r"minos (\S+)", loads)
        expect("the library's minimum macOS", minos.group(1) if minos else None, pins["macos_minimum"])
        exported = set(run(["nm", "-gU", "-j", library]).split())
        expect("exported symbols", sorted(exported), sorted(f"_{n}" for n in names))
        return {"xcode": pins["xcode"], "macos_minimum": pins["macos_minimum"]}
    return {"msvc_toolset": pins["msvc_toolset"]}


def cmd_build(args):
    litert, tools, out = Path(args.litert), Path(args.tools), Path(args.out)
    programs = ("omz_litert_smoke", "omz_litert_plan")
    targets = [PINS["bazel"]["target"], *(f"//{OVERLAY}:{name}" for name in programs)]
    bazel(["build", *build_flags(args.platform), *targets], litert, tools, args.platform, capture=False)
    library = built_file(targets[0], (".so", ".dylib", ".dll"), args)
    expect("library name", library.name, platform_pins(args.platform)["library"])
    out.mkdir(parents=True, exist_ok=True)
    shutil.copy(library, out / library.name)
    (out / "programs").mkdir(exist_ok=True)
    for name, target in zip(programs, targets[1:]):
        program = built_file(target, (".exe", name), args)
        shutil.copy(program, out / "programs" / program.name)
    facts = read_back(args.platform, out / library.name, litert, tools)
    (out / "toolchain.json").write_text(json.dumps(facts, indent=2, sort_keys=True) + "\n")


def repositories(labels: list[str]) -> set[str]:
    """The repositories that contribute C or C++ sources among `labels`; the main repository is `litert`."""
    found = set()
    for label in labels:
        label = label.split(" ")[0]
        if not label.split(":")[-1].endswith(SOURCE_SUFFIXES):
            continue
        found.add(label.lstrip("@").split("//")[0] if label.startswith("@") else "litert")
    return found - {""}


def notices(repos: set[str], licences: dict, roots: dict, rows: Path) -> tuple[str, dict]:
    """THIRD_PARTY_NOTICES.md and the licence files to copy ({archive name: source path}); fails on any gap."""
    problems, files, sections = [], {}, []
    for repo in sorted(repos):
        entry = licences.get(repo)
        if entry is None:
            problems.append(f"{repo}: linked, but has no entry in tools/litert/licences.toml")
            continue
        if not (rows / f"{entry['row']}.toml").is_file():
            problems.append(f"{repo}: its row docs/licensing/deps/{entry['row']}.toml does not exist")
        texts = []
        for name in entry["files"]:
            source = roots[repo] / name
            if not source.is_file():
                problems.append(f"{repo}: licence file {name} is missing from the tree that was built")
                continue
            files[f"{repo}-{name.replace('/', '-')}.txt"] = source  # flat: licences/ has no subfolders
            texts.append(f"#### {name}\n\n```\n{source.read_text(encoding='utf-8', errors='replace').rstrip()}\n```\n")
        sections.append(f"## {entry['name']}\n\nBazel repository `{repo}`, {entry['spdx']}.\n\n" + "\n".join(texts))
    if problems:
        raise Mismatch("notices:\n  " + "\n  ".join(problems))
    head = ("# Third-party notices\n\nThe `omuzaki_litert` library is built from LiteRT "
            f"{PINS['litert']['tag']} (commit {PINS['litert']['commit']}) and links the code below, each at the "
            "version LiteRT's Bazel workspace pins. Their licence files are also in `licences/`.\n\n")
    return head + "\n".join(sections), files


def cmd_notices(args):
    litert, tools, out = Path(args.litert), Path(args.tools), Path(args.out)
    query = ["cquery", *build_flags(args.platform), "--notool_deps", "--noimplicit_deps", "--output=label",
             f"deps({PINS['bazel']['target']})"]
    repos = repositories(bazel(query, litert, tools, args.platform).splitlines())
    output_base = bazel(["info", *build_flags(args.platform), "output_base"], litert, tools, args.platform).strip()
    external = Path(output_base) / "external"
    roots = {repo: litert if repo == "litert" else external / repo for repo in repos}
    licences = tomllib.loads((HERE / "licences.toml").read_text(encoding="utf-8"))["repo"]
    text, files = notices(repos, licences, roots, HERE.parent.parent / "docs/licensing/deps")
    (out / "licences").mkdir(parents=True, exist_ok=True)
    (out / "THIRD_PARTY_NOTICES.md").write_text(text, encoding="utf-8")
    for name, source in files.items():
        shutil.copy(source, out / "licences" / name)
    print("notices:", ", ".join(sorted(repos)))


def archive_bytes(root: str, files: dict, kind: str) -> bytes:
    """A reproducible archive of `files` ({path in the archive: bytes}) under the folder `root`."""
    buffer = io.BytesIO()
    if kind == "zip":
        with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
            for name in sorted(files):
                info = zipfile.ZipInfo(f"{root}/{name}", date_time=(1980, 1, 1, 0, 0, 0))
                info.external_attr = 0o644 << 16
                archive.writestr(info, files[name], zipfile.ZIP_DEFLATED)
        return buffer.getvalue()
    with tarfile.open(fileobj=buffer, mode="w:gz", format=tarfile.PAX_FORMAT, compresslevel=9) as archive:
        for name in sorted(files):
            info = tarfile.TarInfo(f"{root}/{name}")
            info.size, info.mode, info.mtime = len(files[name]), 0o644, 0
            archive.addfile(info, io.BytesIO(files[name]))
    # gzip writes the time into its header; zero it so the same inputs give the same bytes.
    data = bytearray(buffer.getvalue())
    data[4:8] = b"\0\0\0\0"
    return bytes(data)


def stem(plat: str) -> str:
    """The archive's name without extension, which is also its top folder."""
    return f"omuzaki-litert-{PINS['litert']['version']}-{plat}"


def buildinfo(plat: str, toolchain: dict) -> dict:
    pins = platform_pins(plat)
    return {"litert": {key: PINS["litert"][key] for key in ("version", "tag", "commit", "tflite_version")},
            "platform": plat, "omz_litert_abi": PINS["shim"]["abi"], "library": pins["library"],
            "bazel": PINS["bazel"]["version"], "flags": pinned_flags(plat), "toolchain": toolchain,
            "run": {"repository": os.environ.get("GITHUB_REPOSITORY", ""), "id": os.environ.get("GITHUB_RUN_ID", ""),
                    "commit": os.environ.get("GITHUB_SHA", "")}}


def version_tuple(version: str) -> tuple:
    return tuple(int(part) for part in version.split("."))


def buildinfo_problems(info: dict, plat: str) -> list[str]:
    """Every field of an archive's BUILDINFO.json that differs from the pins (design §8.2, §8.4); the run is not checked.
    The toolchain values are the ones `read_back` recorded: each must equal its pin, except the glibc floor, which may
    be older than the pinned one."""
    expected, problems = buildinfo(plat, {}), []
    for field in ("litert", "platform", "omz_litert_abi", "library", "bazel", "flags"):
        if info.get(field) != expected[field]:
            problems.append(f"{field}: archive has {info.get(field)!r}, pinned {expected[field]!r}")
    pins, toolchain = platform_pins(plat), info.get("toolchain")
    keys = {"linux-x64": ("compiler", "glibc"), "macos-arm64": ("xcode", "macos_minimum"),
            "win-x64": ("msvc_toolset",)}[plat]
    if not isinstance(toolchain, dict) or sorted(toolchain) != sorted(keys):
        return problems + [f"toolchain: archive has {toolchain!r}, expected the keys {sorted(keys)}"]
    for key in keys:
        found = toolchain[key]
        if key == "glibc":
            if not re.fullmatch(r"\d+(\.\d+)+", str(found)) or version_tuple(found) > version_tuple(pins["glibc_max"]):
                problems.append(f"toolchain.glibc: archive needs {found!r}, pinned floor {pins['glibc_max']!r}")
        elif found != pins[key]:
            problems.append(f"toolchain.{key}: archive has {found!r}, pinned {pins[key]!r}")
    return problems


def cmd_pack(args):
    out, plat = Path(args.out), platform_pins(args.platform)
    info = buildinfo(args.platform, json.loads((out / "toolchain.json").read_text()))
    root = stem(args.platform)
    files = {f"lib/{plat['library']}": (out / plat["library"]).read_bytes(),
             "LICENSE": (Path(args.litert) / "LICENSE").read_bytes(),
             "THIRD_PARTY_NOTICES.md": (out / "THIRD_PARTY_NOTICES.md").read_bytes(),
             "BUILDINFO.json": (json.dumps(info, indent=2, sort_keys=True) + "\n").encode()}
    for licence in sorted((out / "licences").iterdir()):
        files[f"licences/{licence.name}"] = licence.read_bytes()
    dist = out / "dist"
    dist.mkdir(exist_ok=True)
    asset = dist / f"{root}.{plat['archive']}"
    asset.write_bytes(archive_bytes(root, files, plat["archive"]))
    (dist / "SHA256SUMS").write_text(f"{sha256(asset)}  {asset.name}\n")
    licences = ", ".join(sorted(name.removeprefix("licences/") for name in files if name.startswith("licences/")))
    write_summary(f"### Built {asset.name}\n\nLibrary {len(files['lib/' + plat['library']])} bytes; toolchain "
                  f"{info['toolchain']}.\n\nNotices: {licences}\n")


def archive_in(dist: Path, plat: str) -> Path:
    """The platform's archive in `dist`, which must hold exactly it and its SHA256SUMS line, and match it."""
    asset = dist / f"{stem(plat)}.{platform_pins(plat)['archive']}"
    expect(f"files in {dist.name}", sorted(p.name for p in dist.iterdir()), sorted([asset.name, "SHA256SUMS"]))
    expect(f"SHA256SUMS of {asset.name}", (dist / "SHA256SUMS").read_text(), f"{sha256(asset)}  {asset.name}\n")
    return asset


def cmd_digest(args):
    """Hashes the archive, built or restored from the cache, for the collect job and the run summary."""
    asset = archive_in(Path(args.dist), args.platform)
    digest, size = sha256(asset), asset.stat().st_size
    Path(args.digests).mkdir(parents=True, exist_ok=True)
    (Path(args.digests) / f"{args.platform}.sha256").write_text(digest + "\n")
    write_summary(f"### {asset.name}\n\n| | |\n|---|---|\n| size | {size} bytes |\n| sha256 | `{digest}` |\n")
    write_output("artifact", f"litert-{PINS['litert']['version']}-{args.platform}")


def merge_digests(folder: Path) -> dict:
    """{platform: SHA-256} from one `<platform>.sha256` file per pinned platform; anything else fails."""
    expected = sorted(f"{plat}.sha256" for plat in PINS["platform"])
    expect(f"digest files in {folder.name}", sorted(p.name for p in folder.iterdir()), expected)
    digests = {}
    for plat in PINS["platform"]:
        digest = (folder / f"{plat}.sha256").read_text().strip()
        if not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise Mismatch(f"{plat}.sha256 holds {digest!r}, not a SHA-256")
        digests[plat] = digest
    return digests


def cmd_collect(args):
    digests = merge_digests(Path(args.digests))
    for plat, digest in digests.items():
        write_output(f"sha256_{plat.replace('-', '_')}", digest)
    write_output("sha256", json.dumps(digests, sort_keys=True, separators=(",", ":")))


def cache_key(folder: Path, plat: str) -> tuple[str, bool]:
    """(key, whether the build restored it) as the build job wrote them; a key for another platform fails."""
    key = (folder / "key").read_text().strip()
    if not re.fullmatch(rf"litert-v\d+-{re.escape(plat)}-[\w.-]+-[0-9a-f]{{64}}", key):
        raise Mismatch(f"cache key {key!r} is not a litert key for {plat}")
    hit = (folder / "hit").read_text().strip()
    if hit not in ("true", "false"):
        raise Mismatch(f"cache hit flag {hit!r} is neither true nor false")
    return key, hit == "true"


def cmd_verify(args):
    """Before the cache save, which runs only after every check passed: the bytes about to be saved are the checked
    ones, and the key is the one the build restored from."""
    check_digest(archive_in(Path(args.dist), args.platform), os.environ.get("EXPECTED_SHA256", ""))
    key, hit = cache_key(Path(args.cachekey), args.platform)
    write_output("key", key)
    write_output("hit", "true" if hit else "false")


def cmd_container(args):
    print(platform_pins(args.platform).get("container", ""))


def write_summary(text: str):
    print(text)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as fh:
            fh.write(text + "\n")


def write_output(key: str, value: str):
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as fh:
            fh.write(f"{key}={value}\n")


def check_digest(asset: Path, digest: str):
    """`asset` against `digest`, the build job's; an empty or malformed digest fails."""
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise Mismatch(f"no valid SHA-256 from the build job for {asset.name}: {digest!r}")
    expect(f"SHA-256 of {asset.name}", sha256(asset), digest)


def unpack(asset: Path, digest: str, into: Path) -> Path:
    """Checks `asset` against `digest` (check_digest) and unpacks it; returns its folder."""
    check_digest(asset, digest)
    if asset.name.endswith(".zip"):
        with zipfile.ZipFile(asset) as archive:
            archive.extractall(into)
    else:
        with tarfile.open(asset) as archive:
            archive.extractall(into, filter="data")
    return into / asset.name.removesuffix(".zip").removesuffix(".tar.gz")


def delegated(printed: str) -> tuple[int, int, int]:
    """(nodes taken, nodes in the graph, partitions) from omz_litert_plan's output; fails without its line."""
    found = PLAN.search(printed)
    if not found:
        raise Mismatch(f"omz_litert_plan printed no count: {printed.strip()}")
    taken, total, partitions = (int(group) for group in found.groups())
    return taken, total, partitions


def max_abs_diff(ours, reference, what: str) -> float:
    """The largest |ours - reference|; a NaN or infinity on either side fails, since NaN compares false to any gate."""
    import numpy as np  # installed by the check job

    for side, values in (("ours", ours), ("the wheel's", reference)):
        if not np.all(np.isfinite(values)):
            raise Mismatch(f"{what}: {side} has a non-finite value")
    return float(np.max(np.abs(ours.astype(np.float64) - reference.astype(np.float64)), initial=0.0))


def seeded_input(graph: str, shape, dtype, rng):
    """Inputs in each graph's working range, drawn as the converter's check draws them (convert_mediapipe_face.py)."""
    if graph == "face_blendshapes":
        return (rng.random(shape) * 256).astype(dtype)
    values = rng.random(shape).astype(dtype)
    return values * 2 - 1 if graph == "face_detector" else values


def cmd_check(args):
    import numpy as np  # installed by the check job from requirements-check.txt
    from ai_edge_litert.interpreter import Interpreter

    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    folder = unpack(archive_in(Path(args.dist), args.platform), os.environ.get("EXPECTED_SHA256", ""), work)
    info = json.loads((folder / "BUILDINFO.json").read_text())
    problems = buildinfo_problems(info, args.platform)
    if problems:
        raise Mismatch("BUILDINFO.json differs from tools/litert/pins.toml:\n  " + "\n  ".join(problems))
    library = folder / "lib" / platform_pins(args.platform)["library"]
    programs = Path(args.programs)
    executable = ".exe" if os.name == "nt" else ""
    for program in programs.iterdir():
        program.chmod(0o755)  # artifacts don't keep the executable bit
    smoke = [programs / f"omz_litert_smoke{executable}", library, HERE / "symbols.txt"]
    loaded = dict(line.split(" ", 1) for line in run(smoke).splitlines())
    expect("TfLiteVersion()", loaded["version"], PINS["litert"]["tflite_version"])
    expect("omz_litert_abi()", int(loaded["abi"]), PINS["shim"]["abi"])

    manifest = json.loads((Path(args.models) / "manifest.json").read_text())
    rows, tolerance = [], PINS["reference"]["tolerance"]
    for graph in FACE_GRAPHS:
        model = Path(args.models) / f"{graph}.tflite"
        expect(f"SHA-256 of {model.name}", sha256(model), manifest["files"][model.name]["sha256"])
        reference = Interpreter(model_path=str(model), num_threads=1)
        reference.allocate_tensors()
        rng = np.random.default_rng(0)
        inputs = []
        for index, detail in enumerate(reference.get_input_details()):
            tensor = seeded_input(graph, detail["shape"], detail["dtype"], rng)
            reference.set_tensor(detail["index"], tensor)
            path = work / f"{graph}.in{index}"
            path.write_bytes(tensor.tobytes())
            inputs.append(path)
        reference.invoke()
        expected = [reference.get_tensor(detail["index"]) for detail in reference.get_output_details()]
        produced = work / f"{graph}.out"
        printed = dict(line.split(" ", 1) for line in run([*smoke, model, produced, *inputs]).splitlines())
        taken, total, partitions = delegated(run([programs / f"omz_litert_plan{executable}", model]))
        if taken != total or partitions != 1:
            raise Mismatch(f"{graph}: XNNPACK took {taken} of {total} nodes in {partitions} partitions, not all in one")
        ours = produced.read_bytes()
        expect(f"{graph} output bytes", len(ours), sum(tensor.nbytes for tensor in expected))
        worst, offset = 0.0, 0
        for index, tensor in enumerate(expected):
            mine = np.frombuffer(ours, dtype=tensor.dtype, count=tensor.size, offset=offset)
            worst = max(worst, max_abs_diff(mine, tensor.ravel(), f"{graph} output {index}"))
            offset += tensor.nbytes
        if not printed.get("error"):
            raise Mismatch(f"{graph}: omz_litert_capture_errors caught no text for a refused shape")
        rows.append(f"| {graph} | {taken} of {total} | {worst:.3g} | {printed['error'][:80].replace('|', '/')} |")
        if worst > tolerance:
            raise Mismatch(f"{graph}: max |ours - wheel| = {worst:.3g}, over the {tolerance} gate")
    write_summary(f"### Check {args.platform}\n\n`TfLiteVersion()` {loaded['version']}, "
                  f"`omz_litert_abi()` {loaded['abi']}, {loaded['symbols']} symbols resolved.\n\n"
                  "| graph | nodes delegated to XNNPACK | max abs diff vs ai-edge-litert | captured error (bad shape) |\n"
                  "|---|---|---|---|\n"
                  + "\n".join(rows) + "\n")


def main():
    steps = {"setup": (cmd_setup, ("litert", "tools")), "notices": (cmd_notices, ("litert", "tools", "out")),
             "build": (cmd_build, ("litert", "tools", "out")), "pack": (cmd_pack, ("litert", "out")),
             "digest": (cmd_digest, ("dist", "digests")), "collect": (cmd_collect, ("digests",)),
             "container": (cmd_container, ()), "verify": (cmd_verify, ("dist", "cachekey")),
             "check": (cmd_check, ("dist", "programs", "models", "work"))}
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    for name, (_, needs) in steps.items():
        command = commands.add_parser(name)
        if name != "collect":
            command.add_argument("--platform", required=True, choices=sorted(PINS["platform"]))
        for option in needs:
            command.add_argument(f"--{option}", required=True)
    args = parser.parse_args()
    try:
        steps[args.command][0](args)
    except Mismatch as error:
        print(f"::error::{error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
