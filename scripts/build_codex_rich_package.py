#!/usr/bin/env python3
"""Build a Codex package carrying the Rich TUI release identity."""

import json
import os
from pathlib import Path
import sys


sys.path.insert(0, str(Path(__file__).resolve().parent))

from codex_package.cli import archive_package
from codex_package.cli import assemble_package
from codex_package.cli import parse_args as parse_package_args
from codex_rich_cache import local_build_cache
from codex_rich_cleanup import STAGING_MARKER
from codex_rich_cleanup import STAGING_MARKER_TEXT
from codex_rich_version import read_workspace_version
from codex_rich_version import resolve_build_identity
from codex_rich_version import resolve_runtime_client_version


def main() -> int:
    args = parse_package_args()
    package_dir = getattr(args, "package_dir", None)
    if package_dir is None:
        raise RuntimeError("Codex Rich package builds require --package-dir")
    runtime_client_version = resolve_runtime_client_version()
    workspace_version = read_workspace_version()
    _require_workspace_version(args.package_version, workspace_version)
    identity = resolve_build_identity(upstream_client_version=runtime_client_version)
    os.environ["CODEX_RICH_VERSION"] = identity.cli_version
    with local_build_cache(args) as cache:
        package_dir = assemble_package(args)
        metadata_path = package_dir / "codex-package.json"
        with open(metadata_path, encoding="utf-8") as metadata_file:
            metadata = json.load(metadata_file)
        metadata.update(identity.package_metadata())
        with open(metadata_path, "w", encoding="utf-8") as metadata_file:
            json.dump(metadata, metadata_file, indent=2, sort_keys=True)
            metadata_file.write("\n")
        (package_dir / STAGING_MARKER).write_text(STAGING_MARKER_TEXT, encoding="utf-8")

        archive_package(package_dir, args)
        print(f"Codex Rich build identity: {identity.cli_version}")
        try:
            cache.save(package_dir)
        except Exception as error:
            # A cache failure must not hide a successful package build or block installation.
            print(f"Codex Rich cache snapshot skipped: {error}", file=sys.stderr)
    return 0


def _require_workspace_version(package_version: str, workspace_version: str) -> None:
    if package_version != workspace_version:
        raise RuntimeError(
            "Codex Rich --package-version must match the workspace version "
            f"{workspace_version}"
        )


if __name__ == "__main__":
    raise SystemExit(main())
