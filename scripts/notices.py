#!/usr/bin/env python3
"""Writes the third-party notices as plain text, the common layout of such files: one block
a component, with its name and version, its license, where its source is, then its license
texts. Rust crates come first (from cargo-about's JSON), then Go modules (from the index
scripts/third-party-licenses.sh writes of what go-licenses saved), each by name.

Run by scripts/third-party-licenses.sh; copied beside it as scripts/notices.py. Nothing in it
changes per project.

Usage: notices.py OUT RUST_JSON|- GO_INDEX|-
  GO_INDEX: a line a library: its import path, the address of its license at the version
  used, its license, and the directory go-licenses saved it in, separated by tabs.
"""

import json
import re
import sys
from pathlib import Path

RULE = "=" * 80
THIN = "-" * 80
# The files of a library that hold its license and notices, as go-licenses saves them.
TEXT_FILES = re.compile(r"^(licen[cs]e|copying|notice)", re.IGNORECASE)


def fail(message):
    sys.exit(f"notices.py: {message}")


def rust(path):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    texts = {}
    for license in data["licenses"]:
        for user in license["used_by"]:
            texts.setdefault(user["crate"]["id"], []).append(license["text"])
    crates = sorted(data["crates"], key=lambda c: (c["package"]["name"], c["package"]["version"]))
    for crate in crates:
        package = crate["package"]
        name, version = package["name"], package["version"]
        found = texts.get(package["id"])
        if not found:
            fail(f"{name} {version}: cargo-about gave no license text")
        source = package.get("repository") or f"https://crates.io/crates/{name}/{version}"
        yield f"{name} {version} (Rust crate)", crate["license"], source, found


def go(path):
    rows = []
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if line.strip():
            rows.append(line.split("\t"))
    for lib, url, license, saved in sorted(rows):
        files = sorted(p for p in Path(saved).glob("*") if p.is_file() and TEXT_FILES.match(p.name))
        if not files:
            fail(f"{lib}: no license file in {saved}")
        # The address names the version: .../blob/<tag or commit>/LICENSE.
        version = re.search(r"/blob/([^/]+)/", url)
        label = f"{lib} {version.group(1)}" if version else lib
        texts = [f.read_text(encoding="utf-8", errors="replace") for f in files]
        yield f"{label} (Go module)", license, url, texts


def block(title, license, source, texts):
    body = "\n\n".join(t.strip("\n") for t in dict.fromkeys(texts))
    return f"{RULE}\n{title}\nLicense: {license}\nSource: {source}\n{THIN}\n\n{body}\n\n"


def main():
    if len(sys.argv) != 4:
        fail(__doc__.split("Usage: ")[1].splitlines()[0])
    out, rust_json, go_index = sys.argv[1:]
    blocks = []
    if rust_json != "-":
        blocks += [block(*c) for c in rust(rust_json)]
    if go_index != "-":
        blocks += [block(*m) for m in go(go_index)]
    head = (
        "THIRD-PARTY SOFTWARE NOTICES\n\n"
        "This program includes the components listed below, each under the license given\n"
        "with it: Rust crates first, then Go modules, each by name.\n\n"
    )
    Path(out).write_text(head + "".join(blocks), encoding="utf-8")


if __name__ == "__main__":
    main()
