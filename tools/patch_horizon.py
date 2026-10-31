#!/usr/bin/env python3
"""Download the pinned Horizon commit and apply Brahma's patch.

Fast terrain and the first-run asset prepare are not in that public commit.
``patches/horizon.patch`` adds them. The checkout is ``.horizon/`` and is not
committed.
"""

import hashlib
import os
import shutil
import stat
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / ".horizon"
PATCH = ROOT / "patches" / "horizon.patch"
REV = "f4da935e0ffdb681dd06ebdf9c4a26aff6173df5"
URL = "https://github.com/horizon-rust/horizon.git"
STAMP = DEST / ".brahma-patch"


def run(args, cwd):
    print("+", " ".join(args))
    subprocess.run(args, cwd=cwd, check=True)


def stamp_text():
    digest = hashlib.sha256(PATCH.read_bytes()).hexdigest()
    return f"{REV} {digest}\n"


def remove(path: Path):
    def onerror(func, name, _exc):
        os.chmod(name, stat.S_IWRITE)
        func(name)

    if path.exists():
        shutil.rmtree(path, onexc=onerror)


def main():
    if not PATCH.is_file():
        sys.exit(f"missing {PATCH}")
    want = stamp_text()
    if STAMP.is_file() and STAMP.read_text(encoding="utf-8") == want:
        print("Horizon patch already applied.")
        return
    remove(DEST)
    DEST.mkdir(parents=True)
    run(["git", "init"], DEST)
    run(["git", "remote", "add", "origin", URL], DEST)
    run(["git", "fetch", "--depth", "1", "origin", REV], DEST)
    run(["git", "checkout", "--detach", "FETCH_HEAD"], DEST)
    run(["git", "apply", "--whitespace=nowarn", str(PATCH)], DEST)
    generator = (DEST / "crates/horizon-world/src/worldgen/generator.rs").read_text(
        encoding="utf-8"
    )
    prepare = (DEST / "crates/horizon-prepare/src/lib.rs").read_text(encoding="utf-8")
    if "fn from_approximate" not in generator or "fn prepare_generator" not in prepare:
        sys.exit("the patch did not apply the functions Brahma needs")
    STAMP.write_text(want, encoding="utf-8")
    print(f"Patched Horizon {REV[:12]} into {DEST}")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as exc:
        sys.exit(exc.returncode)
