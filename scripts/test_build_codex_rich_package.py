from contextlib import contextmanager
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile


sys.path.insert(0, str(Path(__file__).resolve().parent))
os.environ.setdefault("CODEX_REPO_ROOT", str(Path(__file__).resolve().parent.parent))

import build_codex_rich_package as builder
from codex_rich_version import RichBuildIdentity


class BuildCodexRichPackageTests(unittest.TestCase):
    def setUp(self) -> None:
        self.environment = patch.dict(os.environ)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.package = self.root / "package"
        self.package.mkdir()
        (self.package / "codex-package.json").write_text('{"version":"0.162.0+rich"}\n')
        self.identity = RichBuildIdentity(
            release_version="2026.08.10",
            fork_commit="74432be312345678",
            upstream_commit="d109393270123456",
            dirty=False,
            upstream_client_version="0.162.0",
        )
        self.events = []
        self.cache = Mock()
        self.cache.save.side_effect = lambda _: self.events.append("save")

    @contextmanager
    def cache_context(self, args):
        self.assertEqual(args.cargo_profile, "dev-small")
        self.events.append("restore")
        try:
            yield self.cache
        finally:
            self.events.append("exit")

    @contextmanager
    def setup_builder(self, extra_args=()):
        with (
            patch.object(
                sys,
                "argv",
                [
                    "build_codex_rich_package.py",
                    "--package-dir",
                    str(self.package),
                    "--package-version=0.162.0+rich",
                    *extra_args,
                ],
            ),
            patch.object(
                builder, "resolve_runtime_client_version", return_value="0.162.0"
            ),
            patch.object(
                builder, "read_workspace_version", return_value="0.162.0+rich"
            ),
            patch.object(builder, "resolve_build_identity", return_value=self.identity),
            patch.object(
                builder, "local_build_cache", side_effect=self.cache_context
            ) as cache,
            patch.object(
                builder, "assemble_package", return_value=self.package
            ) as assemble,
        ):
            yield cache, assemble

    def test_restore_precedes_build_and_archive_contains_provenance_before_save(self):
        archive = self.root / "release.zip"
        with self.setup_builder(["--archive-output", str(archive)]) as (_, assemble):

            def build(args):
                self.events.append("build")
                self.assertEqual(
                    os.environ["CODEX_RICH_VERSION"], self.identity.cli_version
                )
                return self.package

            assemble.side_effect = build

            def save(package):
                self.assertEqual(package, self.package)
                with zipfile.ZipFile(archive) as zipped:
                    metadata = json.loads(zipped.read("codex-package.json"))
                    self.assertEqual(
                        metadata["richForkCommit"], self.identity.fork_commit
                    )
                    self.assertFalse(metadata["richWorkingTreeDirty"])
                self.events.append("save")

            self.cache.save.side_effect = save
            self.assertEqual(builder.main(), 0)
        self.assertEqual(self.events, ["restore", "build", "save", "exit"])
        self.cache.save.assert_called_once_with(self.package)
        metadata = json.loads((self.package / "codex-package.json").read_text())
        self.assertEqual(metadata["richUpstreamClientVersion"], "0.162.0")
        self.assertEqual(metadata["version"], "0.162.0+rich")

    def test_build_failure_never_saves_or_archives(self):
        with (
            self.setup_builder() as (_, assemble),
            patch.object(builder, "archive_package") as archive,
        ):
            assemble.side_effect = RuntimeError("compile failed")
            with self.assertRaisesRegex(RuntimeError, "compile failed"):
                builder.main()
        self.cache.save.assert_not_called()
        archive.assert_not_called()
        self.assertEqual(self.events, ["restore", "exit"])

    def test_package_archive_failure_never_evicts_build_cache(self):
        with (
            self.setup_builder(),
            patch.object(
                builder, "archive_package", side_effect=OSError("archive failed")
            ),
        ):
            with self.assertRaisesRegex(OSError, "archive failed"):
                builder.main()
        self.cache.save.assert_not_called()
        self.assertEqual(self.events, ["restore", "exit"])

    def test_cache_save_failure_preserves_successful_package_result(self):
        self.cache.save.side_effect = OSError("disk full")
        with self.setup_builder(), patch("sys.stderr") as stderr:
            self.assertEqual(builder.main(), 0)
            self.assertTrue(stderr.write.called)
        self.assertIn(
            "richForkCommit", (self.package / "codex-package.json").read_text()
        )

    def test_placeholder_is_rejected_before_restore_or_build(self):
        with (
            self.setup_builder() as (cache, assemble),
            patch.object(
                builder,
                "resolve_runtime_client_version",
                side_effect=RuntimeError("workspace version 0.0.0"),
            ),
        ):
            with self.assertRaisesRegex(RuntimeError, "workspace version 0.0.0"):
                builder.main()
        cache.assert_not_called()
        assemble.assert_not_called()

    def test_mismatched_package_versions_are_rejected_before_restore_or_build(self):
        for extra in (
            ["--package-version", "0.162.0"],
            ["--package-version=0.162.0"],
            ["--package-ver=0.162.0"],
        ):
            with (
                self.subTest(extra=extra),
                self.setup_builder(extra) as (cache, assemble),
            ):
                with self.assertRaisesRegex(
                    RuntimeError, "must match the workspace version"
                ):
                    builder.main()
                cache.assert_not_called()
                assemble.assert_not_called()


if __name__ == "__main__":
    unittest.main()
