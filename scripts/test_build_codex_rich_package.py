from pathlib import Path
import os
import sys
import tempfile
import unittest
from unittest.mock import patch


sys.path.insert(0, str(Path(__file__).resolve().parent))
os.environ.setdefault("CODEX_REPO_ROOT", str(Path(__file__).resolve().parent.parent))

import build_codex_rich_package
from codex_rich_version import RichBuildIdentity


class BuildCodexRichPackageTests(unittest.TestCase):
    def test_placeholder_version_is_rejected_before_builder(self) -> None:
        with (
            patch.object(
                sys, "argv", ["build_codex_rich_package.py", "--package-dir", "unused"]
            ),
            patch.object(
                build_codex_rich_package,
                "resolve_runtime_client_version",
                side_effect=RuntimeError(
                    "Codex Rich cannot package workspace version 0.0.0"
                ),
            ),
            patch.object(build_codex_rich_package, "build_codex_package") as builder,
        ):
            with self.assertRaisesRegex(
                RuntimeError, "cannot package workspace version 0.0.0"
            ):
                build_codex_rich_package.main()
        builder.assert_not_called()

    def test_success_records_provenance_and_protects_requested_archive(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            package_dir = Path(directory)
            (package_dir / "codex-package.json").write_text(
                '{"version": "0.162.0+rich"}\n', encoding="utf-8"
            )
            with (
                patch.object(
                    sys,
                    "argv",
                    [
                        "build_codex_rich_package.py",
                        "--package-dir",
                        str(package_dir),
                        "--package-version=0.162.0+rich",
                        "--archive-output",
                        str(package_dir / "release.tar.gz"),
                    ],
                ),
                patch.object(
                    build_codex_rich_package,
                    "resolve_runtime_client_version",
                    return_value="0.162.0",
                ),
                patch.object(
                    build_codex_rich_package,
                    "read_workspace_version",
                    return_value="0.162.0+rich",
                ),
                patch.object(
                    build_codex_rich_package,
                    "resolve_build_identity",
                    return_value=self._identity(),
                ),
                patch.object(
                    build_codex_rich_package, "build_codex_package", return_value=0
                ) as builder,
                patch.object(
                    build_codex_rich_package, "cleanup_after_build"
                ) as cleanup,
            ):
                self.assertEqual(build_codex_rich_package.main(), 0)
            metadata = (package_dir / "codex-package.json").read_text()

        builder.assert_called_once_with()
        cleanup.assert_called_once_with(
            package_dir.resolve(),
            "dev-small",
            protected_paths=((package_dir / "release.tar.gz").resolve(),),
        )
        self.assertIn('"version": "0.162.0+rich"', metadata)
        self.assertIn('"richUpstreamClientVersion": "0.162.0"', metadata)

    def test_failed_build_does_not_clean_caches(self) -> None:
        with (
            patch.object(
                sys,
                "argv",
                [
                    "build_codex_rich_package.py",
                    "--package-dir",
                    "unused",
                    "--package-version=0.162.0+rich",
                ],
            ),
            patch.object(
                build_codex_rich_package,
                "resolve_runtime_client_version",
                return_value="0.162.0",
            ),
            patch.object(
                build_codex_rich_package,
                "read_workspace_version",
                return_value="0.162.0+rich",
            ),
            patch.object(
                build_codex_rich_package,
                "resolve_build_identity",
                return_value=self._identity(),
            ),
            patch.object(
                build_codex_rich_package, "build_codex_package", return_value=7
            ),
            patch.object(build_codex_rich_package, "cleanup_after_build") as cleanup,
        ):
            self.assertEqual(build_codex_rich_package.main(), 7)
        cleanup.assert_not_called()

    def test_cleanup_failure_does_not_hide_successful_release_package(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            package_dir = Path(directory)
            (package_dir / "codex-package.json").write_text("{}\n", encoding="utf-8")
            with (
                patch.object(
                    sys,
                    "argv",
                    [
                        "build_codex_rich_package.py",
                        "--package-dir",
                        directory,
                        "--cargo-profile",
                        "release",
                        "--package-version=0.162.0+rich",
                    ],
                ),
                patch.object(
                    build_codex_rich_package,
                    "resolve_runtime_client_version",
                    return_value="0.162.0",
                ),
                patch.object(
                    build_codex_rich_package,
                    "read_workspace_version",
                    return_value="0.162.0+rich",
                ),
                patch.object(
                    build_codex_rich_package,
                    "resolve_build_identity",
                    return_value=self._identity(),
                ),
                patch.object(
                    build_codex_rich_package, "build_codex_package", return_value=0
                ),
                patch.object(
                    build_codex_rich_package,
                    "cleanup_after_build",
                    side_effect=OSError("cache is busy"),
                ) as cleanup,
                patch("sys.stderr") as stderr,
            ):
                self.assertEqual(build_codex_rich_package.main(), 0)
                cleanup.assert_called_once_with(package_dir.resolve(), "release")
                self.assertTrue(stderr.write.called)
            self.assertIn(
                "richForkCommit", (package_dir / "codex-package.json").read_text()
            )

    def test_mismatched_package_versions_are_rejected_before_builder(self) -> None:
        for package_args in (
            ["--package-version", "0.162.0"],
            ["--package-version=0.162.0"],
            ["--package-ver=0.162.0"],
        ):
            with self.subTest(package_args=package_args):
                with (
                    patch.object(
                        sys,
                        "argv",
                        [
                            "build_codex_rich_package.py",
                            "--package-dir",
                            "unused",
                            *package_args,
                        ],
                    ),
                    patch.object(
                        build_codex_rich_package,
                        "resolve_runtime_client_version",
                        return_value="0.162.0",
                    ),
                    patch.object(
                        build_codex_rich_package,
                        "read_workspace_version",
                        return_value="0.162.0+rich",
                    ),
                    patch.object(
                        build_codex_rich_package, "build_codex_package"
                    ) as builder,
                ):
                    with self.assertRaisesRegex(
                        RuntimeError, "must match the workspace version"
                    ):
                        build_codex_rich_package.main()
                builder.assert_not_called()

    @staticmethod
    def _identity() -> RichBuildIdentity:
        return RichBuildIdentity(
            release_version="2026.08.10",
            fork_commit="74432be312345678",
            upstream_commit="d109393270123456",
            dirty=False,
            upstream_client_version="0.162.0",
        )


if __name__ == "__main__":
    unittest.main()
