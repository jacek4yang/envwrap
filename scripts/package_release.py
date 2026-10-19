#!/usr/bin/env python3
"""Verify a clean checkout and package its Linux x86_64 GNU release. Never upload."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"


def run(*args, **kwargs):
    print("+", " ".join(map(str, args)), flush=True)
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def output(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def check_docs():
    documents = [ROOT / "README.md", ROOT / "CHANGELOG.md", *sorted((ROOT / "docs").rglob("*.md"))]
    for document in documents:
        text = document.read_text()
        if text.count("```") % 2:
            raise SystemExit(f"Unclosed code fence: {document.relative_to(ROOT)}")
        for match in re.finditer(r"\[[^\]\n]*\]\(([^)\s]+)\)", text):
            link = urlsplit(match[1])
            if link.scheme or link.netloc:
                continue
            destination = (document.parent / unquote(link.path)).resolve() if link.path else document
            if not destination.is_relative_to(ROOT) or not destination.is_file():
                raise SystemExit(f"Broken/local-outside-repository link: {document.name}: {match[1]}")
            if link.fragment and destination.suffix == ".md":
                headings = re.findall(r"^#{1,6}\s+(.+)$", destination.read_text(), re.MULTILINE)
                anchors = {re.sub(r"[^\w\- ]", "", title.lower()).replace(" ", "-") for title in headings}
                if unquote(link.fragment) not in anchors:
                    raise SystemExit(f"Missing heading: {document.name}: {match[1]}")
    print(f"PASS local documentation links and fences ({len(documents)} documents)", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-docs", action="store_true", help="only check local documentation links and fences")
    parser.add_argument("--reference-checker", type=Path, help="optional offline checker cross-check in the independent acceptance gate")
    args = parser.parse_args()
    check_docs()
    if args.check_docs:
        return
    if Path(output("git", "rev-parse", "--show-toplevel")).resolve() != ROOT:
        raise SystemExit("Packaging requires this project's own checkout, not an archive inside another repository.")
    if output("git", "status", "--porcelain", "--untracked-files=normal"):
        raise SystemExit("Commit all intended source changes first; release packaging requires a clean checkout.")
    rustc = output("rustc", "-vV")
    if f"host: {TARGET}" not in rustc.splitlines():
        raise SystemExit(f"This packaging path is verified only on {TARGET}; do not relabel an untested target.")
    package = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    version = package["version"]
    commit = output("git", "rev-parse", "HEAD")
    epoch = int(output("git", "show", "-s", "--format=%ct", "HEAD"))
    for command in [
        ("cargo", "fmt", "--all", "--", "--check"),
        ("cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"),
        ("cargo", "test", "--locked", "--all-targets", "--", "--nocapture"),
        ("cargo", "build", "--release", "--locked", "--target", TARGET),
    ]:
        run(*command)
    binary = ROOT / "target" / TARGET / "release" / "envwrap"
    if output(str(binary), "--version") != f"envwrap {version}":
        raise SystemExit("Built version does not match Cargo.toml.")
    # The same independent acceptance is used by developers and release builds.
    sys.dont_write_bytecode = True
    from acceptance import run_checks
    acceptance = run_checks(binary, args.reference_checker)
    if not acceptance["passed"]:
        raise SystemExit("Independent environment/network acceptance failed; no release assets generated.")
    requirements = output("readelf", "--version-info", str(binary))
    glibc_versions = re.findall(r"Name: GLIBC_([0-9.]+)", requirements)
    if not glibc_versions:
        raise SystemExit("Expected a GNU dynamically linked binary; inspect the target before publishing.")
    highest_glibc = max(glibc_versions, key=lambda value: tuple(map(int, value.split("."))))
    info = {
        "name": "envwrap", "version": version, "commit": commit, "target": TARGET,
        "binary_sha256": digest(binary), "rustc": rustc, "cargo": output("cargo", "--version"),
        "build_glibc": output("getconf", "GNU_LIBC_VERSION"), "required_glibc_symbols_up_to": highest_glibc,
        "needed_libraries": re.findall(r"Shared library: \[([^\]]+)\]", output("readelf", "-d", str(binary))),
        "tested_kernel": os.uname().release, "tested_bubblewrap": output("bwrap", "--version"),
        "checks": ["documentation", "fmt", "clippy", "all-targets tests", "release HTTP runtime", "release SOCKS5 runtime", "release SOCKS5h runtime"],
        "acceptance_standard": acceptance["standard"],
        "acceptance_passed": acceptance["passed"],
        "reference_checker": acceptance["reference_checker"],
        "claude_authenticated_acceptance": "not executed; optional application compatibility evidence, not a release gate",
    }
    if output("git", "status", "--porcelain", "--untracked-files=normal"):
        raise SystemExit("Verification changed the checkout; inspect before packaging.")
    if output("git", "rev-parse", "HEAD") != commit:
        raise SystemExit("HEAD changed during verification; rebuild from the intended commit.")
    dist = ROOT / "target/dist"
    dist.mkdir(parents=True, exist_ok=True)
    name = f"envwrap-v{version}-{TARGET}"
    archive = dist / f"{name}.tar.gz"
    with tempfile.TemporaryDirectory(prefix="package-", dir=dist) as temporary:
        stage = Path(temporary) / name
        stage.mkdir()
        shutil.copy2(binary, stage / "envwrap")
        (stage / "ACCEPTANCE.json").write_text(json.dumps(acceptance, indent=2) + "\n")
        for filename in ("README.md", "LICENSE", "CHANGELOG.md"):
            shutil.copy2(ROOT / filename, stage / filename)
        shutil.copytree(ROOT / "docs", stage / "docs")
        (stage / "BUILD-INFO.json").write_text(json.dumps(info, indent=2) + "\n")
        with archive.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|") as tar:
                for path in [stage, *sorted(stage.rglob("*"))]:
                    if path.is_symlink():
                        raise SystemExit("Release documents must not contain symlinks.")
                    entry = tar.gettarinfo(str(path), arcname=str(path.relative_to(stage.parent)))
                    entry.uid = entry.gid = 0
                    entry.uname = entry.gname = ""
                    entry.mtime = epoch
                    entry.mode = 0o755 if path.is_dir() or path == stage / "envwrap" else 0o644
                    if path.is_file():
                        with path.open("rb") as stream:
                            tar.addfile(entry, stream)
                    else:
                        tar.addfile(entry)
    source = dist / f"envwrap-v{version}-source.tar.gz"
    with source.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
        with subprocess.Popen(
            ["git", "archive", "--format=tar", f"--prefix=envwrap-v{version}/", commit],
            cwd=ROOT, stdout=subprocess.PIPE,
        ) as process:
            shutil.copyfileobj(process.stdout, compressed)
            if process.wait() != 0:
                raise SystemExit("git archive failed")
    sums = "".join(f"{digest(path)}  {path.name}\n" for path in (archive, source))
    (dist / "SHA256SUMS").write_text(sums)
    print(json.dumps(info, indent=2))
    print(sums, end="")
    print(f"Release assets ready in {dist}; nothing has been uploaded or installed.")


if __name__ == "__main__":
    main()
