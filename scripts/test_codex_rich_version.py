from datetime import datetime
from pathlib import Path
import sys
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parent))

from codex_rich_version import resolve_build_identity


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


if __name__ == "__main__":
    unittest.main()
