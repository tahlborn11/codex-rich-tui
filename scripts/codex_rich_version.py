"""Build identity helpers for the Codex Rich TUI fork."""

from dataclasses import dataclass
from datetime import datetime
import os
from pathlib import Path
import re
import subprocess
from zoneinfo import ZoneInfo


REPO_ROOT = Path(__file__).resolve().parent.parent
RELEASE_VERSION_PATTERN = re.compile(r"^\d{4}\.\d{2}\.\d{2}(?:\.\d+)?$")


@dataclass(frozen=True)
class RichBuildIdentity:
    release_version: str
    fork_commit: str
    upstream_commit: str
    dirty: bool

    @property
    def cli_version(self) -> str:
        fork_revision = self.fork_commit[:8]
        if self.dirty:
            fork_revision = f"{fork_revision}.dirty"
        return (
            f"{self.release_version}+{fork_revision} "
            f"(upstream {self.upstream_commit[:8]})"
        )

    def package_metadata(self) -> dict[str, object]:
        return {
            "richVersion": self.release_version,
            "richBuildVersion": self.cli_version,
            "richForkCommit": self.fork_commit,
            "richUpstreamCommit": self.upstream_commit,
            "richWorkingTreeDirty": self.dirty,
        }


def resolve_build_identity(
    *,
    repo_root: Path = REPO_ROOT,
    environ: dict[str, str] | None = None,
    now: datetime | None = None,
) -> RichBuildIdentity:
    environment = os.environ if environ is None else environ
    release_version = environment.get("CODEX_RICH_RELEASE_VERSION")
    if release_version is None:
        current_time = now or datetime.now(ZoneInfo("America/New_York"))
        release_version = current_time.strftime("%Y.%m.%d")
    if RELEASE_VERSION_PATTERN.fullmatch(release_version) is None:
        raise RuntimeError(
            "CODEX_RICH_RELEASE_VERSION must use YYYY.MM.DD or YYYY.MM.DD.N"
        )
    try:
        datetime.strptime(".".join(release_version.split(".")[:3]), "%Y.%m.%d")
    except ValueError as error:
        raise RuntimeError(
            "CODEX_RICH_RELEASE_VERSION must contain a valid calendar date"
        ) from error

    fork_commit = environment.get("CODEX_RICH_FORK_COMMIT") or _git_output(
        repo_root, "rev-parse", "HEAD"
    )
    upstream_commit = environment.get("CODEX_RICH_UPSTREAM_COMMIT") or _git_output(
        repo_root, "rev-parse", "upstream/main"
    )
    dirty_override = environment.get("CODEX_RICH_WORKING_TREE_DIRTY")
    if dirty_override is None:
        dirty = bool(_git_output(repo_root, "status", "--porcelain"))
    elif dirty_override == "1":
        dirty = True
    elif dirty_override == "0":
        dirty = False
    else:
        raise RuntimeError("CODEX_RICH_WORKING_TREE_DIRTY must be 0 or 1")

    return RichBuildIdentity(
        release_version=release_version,
        fork_commit=fork_commit,
        upstream_commit=upstream_commit,
        dirty=dirty,
    )


def _git_output(repo_root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=repo_root,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()
