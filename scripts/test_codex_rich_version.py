from datetime import datetime
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parent))

from codex_rich_version import resolve_build_identity
from codex_rich_version import resolve_runtime_client_version


class RichBuildIdentityTests(unittest.TestCase):
    def test_explicit_clean_identity(self) -> None:
        identity = resolve_build_identity(
            repo_root=Path("unused"),
            environ={
                "CODEX_RICH_RELEASE_VERSION": "2026.08.10",
                "CODEX_RICH_FORK_COMMIT": "74432be312345678",
                "CODEX_RICH_UPSTREAM_COMMIT": "d109393270123456",
                "CODEX_RICH_WORKING_TREE_DIRTY": "0",
            },
        )

        self.assertEqual(
            identity.cli_version,
            "2026.08.10+74432be3 (upstream d1093932)",
        )
        self.assertEqual(
            identity.package_metadata(),
            {
                "richVersion": "2026.08.10",
                "richBuildVersion": "2026.08.10+74432be3 (upstream d1093932)",
                "richForkCommit": "74432be312345678",
                "richUpstreamCommit": "d109393270123456",
                "richWorkingTreeDirty": False,
            },
        )

    def test_default_upstream_commit_is_merge_base(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repo_root = Path(directory)
            self._git(repo_root, "init")
            self._git(repo_root, "config", "user.email", "test@example.com")
            self._git(repo_root, "config", "user.name", "Test User")
            self._commit(repo_root, "base")
            base = self._git(repo_root, "rev-parse", "HEAD")
            fork_branch = self._git(repo_root, "branch", "--show-current")
            self._git(repo_root, "branch", "upstream/main")
            self._commit(repo_root, "fork change")
            self._git(repo_root, "checkout", "upstream/main")
            self._commit(repo_root, "new upstream change")
            upstream_tip = self._git(repo_root, "rev-parse", "HEAD")
            self._git(repo_root, "checkout", fork_branch)

            identity = resolve_build_identity(
                repo_root=repo_root,
                environ={
                    "CODEX_RICH_RELEASE_VERSION": "2026.08.10",
                    "CODEX_RICH_WORKING_TREE_DIRTY": "0",
                },
            )

        self.assertEqual(identity.upstream_commit, base)
        self.assertNotEqual(identity.upstream_commit, upstream_tip)

    def test_default_version_uses_new_york_date_and_marks_dirty_build(self) -> None:
        identity = resolve_build_identity(
            repo_root=Path("unused"),
            environ={
                "CODEX_RICH_FORK_COMMIT": "abcdef1234567890",
                "CODEX_RICH_UPSTREAM_COMMIT": "1234567890abcdef",
                "CODEX_RICH_WORKING_TREE_DIRTY": "1",
            },
            now=datetime(2026, 8, 11, 2, 30),
        )

        self.assertEqual(
            identity.cli_version,
            "2026.08.11+abcdef12.dirty (upstream 12345678)",
        )

    def test_invalid_release_version_is_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "YYYY.MM.DD"):
            resolve_build_identity(
                repo_root=Path("unused"),
                environ={"CODEX_RICH_RELEASE_VERSION": "v1"},
            )

    def test_invalid_calendar_date_is_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "valid calendar date"):
            resolve_build_identity(
                repo_root=Path("unused"),
                environ={"CODEX_RICH_RELEASE_VERSION": "2026.99.99"},
            )

    def test_runtime_client_version_rejects_placeholder_versions(self) -> None:
        for version in ("0.0.0", "0.0.0+rich", "0.0.0-alpha.1"):
            with self.subTest(version=version):
                with self.assertRaisesRegex(
                    RuntimeError, "cannot package workspace version 0.0.0"
                ):
                    self._runtime_client_version(version)

    def test_runtime_client_version_uses_whole_semver(self) -> None:
        self.assertEqual(self._runtime_client_version("0.162.0+rich"), "0.162.0")

    def test_invalid_workspace_semver_is_rejected(self) -> None:
        for version in ("0.162", "0.162.0-alpha.01", "18446744073709551616.1.0"):
            with self.subTest(version=version):
                with self.assertRaisesRegex(RuntimeError, "semantic workspace version"):
                    self._runtime_client_version(version)

    @staticmethod
    def _git(repo_root: Path, *args: str) -> str:
        result = subprocess.run(
            ["git", *args], cwd=repo_root, check=True, capture_output=True, text=True
        )
        return result.stdout.strip()

    def _commit(self, repo_root: Path, message: str) -> None:
        path = repo_root / "history"
        path.write_text(f"{message}\n", encoding="utf-8")
        self._git(repo_root, "add", "history")
        self._git(repo_root, "commit", "-m", message)

    def _runtime_client_version(self, version: str) -> str:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "codex-rs" / "Cargo.toml"
            manifest.parent.mkdir()
            manifest.write_text(
                f'[workspace.package]\nversion = "{version}"\n', encoding="utf-8"
            )
            return resolve_runtime_client_version(repo_root=root)


if __name__ == "__main__":
    unittest.main()
