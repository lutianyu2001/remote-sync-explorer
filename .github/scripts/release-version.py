#!/usr/bin/env python3
"""Gives a build the version named in RELEASE_VERSION, in place of the one its manifests hold.

Copied into a project as .github/scripts/release-version.py and run in the checkout a release
is built from, before the build: release.yml sets RELEASE_VERSION to the tag for a pre-release
(v1.2.0-rc.1), whose version no commit writes into the manifests, and leaves it empty for a
release, whose manifests already hold the tag's version. Nothing here is per project.

Without RELEASE_VERSION, or with an empty one, nothing is changed. With it (a leading v is
dropped), the version is rewritten in each of these the directory has:
  Cargo.toml      the package's version, or the workspace's ([workspace.package]), with the
                  members that share it, and their entries in Cargo.lock, so --locked holds
  pyproject.toml  [project]'s version, when it is stated there
  build.zig.zon   .version
The files are changed in place and never committed: run it in a checkout made for the build.

Usage: RELEASE_VERSION=<version> release-version.py   (in the checkout's root)
Exit status: 0 done, or nothing asked for; 1 a manifest whose version could not be rewritten.
"""
import json
import os
import re
import subprocess
import sys


def rewrite(path, pattern, new):
    """Replaces group 2 of the first match of `pattern` in `path` with `new`."""
    with open(path, encoding="utf-8") as f:
        text = f.read()
    text, count = re.subn(
        pattern, lambda m: m.group(1) + new + m.group(3), text, count=1, flags=re.M)
    if count:
        with open(path, "w", encoding="utf-8") as f:
            f.write(text)
    return bool(count)


def cargo(new):
    with open("Cargo.toml", encoding="utf-8") as f:
        root = f.read()
    # The first version line of [package] or [workspace.package], whichever the root has.
    table = re.search(
        r'^\[(?:workspace\.)?package\]\n(?:(?!\[).*\n)*?version\s*=\s*"([^"]*)"', root, re.M)
    if not table:
        sys.exit("Cargo.toml: no version in [package] or [workspace.package]")
    old = table.group(1)
    line = r'^(version\s*=\s*")(' + re.escape(old) + r')(")'
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"]))
    rewrite("Cargo.toml", line, new)
    for package in meta["packages"]:
        if package["version"] != old:
            continue
        # A member that states the same version itself, rather than inheriting it.
        if os.path.abspath(package["manifest_path"]) != os.path.abspath("Cargo.toml"):
            rewrite(package["manifest_path"], line, new)
        entry = (r'^(name = "' + re.escape(package["name"]) + r'"\nversion = ")('
                 + re.escape(old) + r')(")')
        if os.path.exists("Cargo.lock") and not rewrite("Cargo.lock", entry, new):
            sys.exit(f"Cargo.lock: no entry of {package['name']} {old}")
    return old


def pyproject(new):
    with open("pyproject.toml", encoding="utf-8") as f:
        text = f.read()
    # Dynamic (maturin takes Cargo.toml's, a plugin its own source): nothing stated here.
    table = re.search(r'^\[project\]\n(?:(?!\[).*\n)*?version\s*=\s*"([^"]*)"', text, re.M)
    if not table:
        return None
    old = table.group(1)
    rewrite("pyproject.toml",
            r'(^\[project\]\n(?:(?!\[).*\n)*?version\s*=\s*")(' + re.escape(old) + r')(")', new)
    return old


def zig(new):
    with open("build.zig.zon", encoding="utf-8") as f:
        found = re.search(r'^\s*\.version = "([^"]*)",', f.read(), re.M)
    if not found:
        sys.exit("build.zig.zon: no .version")
    rewrite("build.zig.zon", r'^(\s*\.version = ")(' + re.escape(found.group(1)) + r')(",)', new)
    return found.group(1)


def main():
    new = os.environ.get("RELEASE_VERSION", "").removeprefix("v")
    if not new:
        return
    if not re.fullmatch(r"[0-9][0-9A-Za-z.+-]*", new):
        sys.exit(f"RELEASE_VERSION: {new!r} is not a version")
    manifests = (("Cargo.toml", cargo), ("pyproject.toml", pyproject), ("build.zig.zon", zig))
    seen = False
    for name, set_version in manifests:
        if os.path.exists(name):
            seen = True
            old = set_version(new)
            if old is not None:
                print(f"{name}: {old} -> {new}")
    if not seen:
        sys.exit("no Cargo.toml, pyproject.toml or build.zig.zon here")


if __name__ == "__main__":
    main()
