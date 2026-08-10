#!/usr/bin/env python3
"""Build a Codex package carrying the Rich TUI release identity."""

import json
import os
from pathlib import Path
import sys


sys.path.insert(0, str(Path(__file__).resolve().parent))

from codex_package.cli import main as build_codex_package
from codex_rich_version import resolve_build_identity


def main() -> int:
    package_dir = _package_dir_from_args(sys.argv[1:])
    identity = resolve_build_identity()
    os.environ["CODEX_RICH_VERSION"] = identity.cli_version
    result = build_codex_package()
    if result != 0:
        return result

    metadata_path = package_dir.resolve() / "codex-package.json"
    with open(metadata_path, encoding="utf-8") as metadata_file:
        metadata = json.load(metadata_file)
    metadata.update(identity.package_metadata())
    with open(metadata_path, "w", encoding="utf-8") as metadata_file:
        json.dump(metadata, metadata_file, indent=2, sort_keys=True)
        metadata_file.write("\n")

    print(f"Codex Rich build identity: {identity.cli_version}")
    return 0


def _package_dir_from_args(args: list[str]) -> Path:
    for index, arg in enumerate(args):
        if arg == "--package-dir" and index + 1 < len(args):
            return Path(args[index + 1])
        if arg.startswith("--package-dir="):
            return Path(arg.partition("=")[2])
    raise RuntimeError("Codex Rich package builds require --package-dir")


if __name__ == "__main__":
    raise SystemExit(main())
