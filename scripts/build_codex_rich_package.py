#!/usr/bin/env python3
"""Build a Codex package carrying the Rich TUI release identity."""

import json
import os
from pathlib import Path
import sys


sys.path.insert(0, str(Path(__file__).resolve().parent))

from codex_package.cli import main as build_codex_package
from codex_package.cli import parse_args as parse_package_args
from codex_rich_cleanup import cleanup_after_build
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
    (package_dir / STAGING_MARKER).write_text(STAGING_MARKER_TEXT, encoding="utf-8")

    print(f"Codex Rich build identity: {identity.cli_version}")
    try:
        if args.archive_output:
            cleanup_after_build(
                package_dir.resolve(),
                args.cargo_profile,
                protected_paths=tuple(path.resolve() for path in args.archive_output),
            )
        else:
            cleanup_after_build(package_dir.resolve(), args.cargo_profile)
    except Exception as error:
        # Cache maintenance must not hide a successful package build or prevent installation.
        print(f"Codex Rich cache cleanup skipped: {error}", file=sys.stderr)
    return 0


def _require_workspace_version(package_version: str, workspace_version: str) -> None:
    if package_version != workspace_version:
        raise RuntimeError(
            "Codex Rich --package-version must match the workspace version "
            f"{workspace_version}"
        )


if __name__ == "__main__":
    raise SystemExit(main())
