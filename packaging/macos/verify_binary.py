#!/usr/bin/env python3
"""Reject a binary whose architecture/deployment target violates the bundle target."""
import argparse
import pathlib
import re
import subprocess


def minimum_versions(loads):
    versions = []
    for command in re.split(r"\nLoad command \d+\n", loads):
        if re.search(r"\bcmd LC_VERSION_MIN_MACOSX\b", command):
            versions.extend(re.findall(r"^\s*version (\d+\.\d+(?:\.\d+)?)", command, re.MULTILINE))
        elif re.search(r"\bcmd LC_BUILD_VERSION\b", command):
            versions.extend(re.findall(r"^\s*minos (\d+\.\d+(?:\.\d+)?)", command, re.MULTILINE))
    return versions


def verify(path, target):
    arch = "x86_64" if target.startswith("x86_64") else "arm64"
    actual = subprocess.check_output(["lipo", "-archs", str(path)], text=True).strip()
    if actual != arch:
        raise SystemExit(f"architecture mismatch: {actual} != {arch}")
    loads = subprocess.check_output(["otool", "-l", str(path)], text=True)
    versions = minimum_versions(loads)
    if not versions:
        raise SystemExit("no minimum OS load command found")
    maximum = (10, 13) if arch == "x86_64" else (11, 0)
    for version in versions:
        numbers = tuple(map(int, version.split(".")))
        if numbers[:2] > maximum:
            raise SystemExit(f"minimum OS too new: {version} > {maximum}")
    dependencies = subprocess.check_output(["otool", "-L", str(path)], text=True).splitlines()[1:]
    for line in dependencies:
        dependency = line.strip().split(" (")[0]
        if not dependency.startswith(("/System/Library/", "/usr/lib/")):
            raise SystemExit(f"non-system dependency not bundled: {dependency}")
    print(f"PASS: {arch}; deployment target {versions}; system libraries only")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    verify(args.binary, args.target)
