#!/usr/bin/env python3
"""Conservative, best-effort cleanup for local Codex Rich Cargo artifacts.

This module deliberately recognizes only the small set of directories produced by
the package build.  It never follows symlinks, and treats an unreadable tree as
unsafe rather than empty.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import errno
import json
import math
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import time
from typing import Iterable


source_repo_root = Path(__file__).resolve().parents[1]
_PROFILES = ("debug", "dev-small", "release", "ci-test", "profiling")
# Keep this in sync with scripts/codex_package/targets.py.  Importing that module
# requires CODEX_REPO_ROOT, which is intentionally not a cleanup prerequisite.
_TARGETS = (
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
)
_LOCK_NAMES = (".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock")
_STAGING_GRACE_SECONDS = 3600
_WRITER_NAMES = {"cargo", "rustc", "rustdoc", "clippy-driver", "cargo-nextest"}
_VARIANT_ENTRYPOINTS = {
    "codex": "codex",
    "codex-app-server": "codex-app-server",
}
STAGING_MARKER = ".codex-rich-staging"
STAGING_MARKER_TEXT = "codex-rich-staging-v1\n"
_INSTALLED_MARKER = ".codex-rich-installed"
_CACHE_DIRS = {"deps", "build", ".fingerprint", "incremental", "examples"}


@dataclass(frozen=True)
class Limits:
    max_bytes: int
    keep_seconds: float
    min_free_bytes: int


@dataclass
class CleanupReport:
    deleted: list[Path] = field(default_factory=list)
    bytes_freed: int = 0
    projected_free_bytes: int | None = None
    unmet: list[str] = field(default_factory=list)
    cold_fallback: bool = False
    skipped: str | None = None
    before_bytes: int | None = None
    retained_bytes: int | None = None
    failed: list[Path] = field(default_factory=list)


@dataclass(frozen=True)
class Candidate:
    path: Path
    kind: str  # staging, incremental, profile
    current: bool
    mtime: float
    size: int


def _number(name: str, default: str) -> float:
    raw = os.environ.get(name, default)
    try:
        value = float(raw)
    except ValueError as exc:
        raise ValueError(
            f"{name} must be a finite nonnegative number, got {raw!r}"
        ) from exc
    if not math.isfinite(value) or value < 0:
        raise ValueError(f"{name} must be a finite nonnegative number, got {raw!r}")
    return value


def read_limits() -> Limits:
    gib = 1024**3
    return Limits(
        max_bytes=int(_number("CODEX_RICH_CACHE_MAX_GIB", "50") * gib),
        keep_seconds=_number("CODEX_RICH_CACHE_KEEP_DAYS", "14") * 86400,
        min_free_bytes=int(_number("CODEX_RICH_MIN_FREE_GIB", "100") * gib),
    )


def _is_relative_to(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def _safe_root(repo_root: Path) -> Path | None:
    """Reject symlinks at the repository boundary and within its target path."""
    try:
        repo = repo_root.absolute()
        target = repo / "codex-rs" / "target"
        # Do not resolve first: that would hide a symlink below the repository
        # boundary, including a parent of target.
        for current in (repo, repo / "codex-rs", target):
            if current.exists() and stat.S_ISLNK(os.lstat(current).st_mode):
                return None
        if not target.is_dir() or stat.S_ISLNK(os.lstat(target).st_mode):
            return None
        resolved_repo = repo.resolve(strict=True)
        resolved_target = target.resolve(strict=True)
        if not _is_relative_to(resolved_target, resolved_repo / "codex-rs"):
            return None
        return target
    except (OSError, RuntimeError):
        return None


def _has_nested_mount(path: Path) -> bool:
    """Linux bind mounts can share st_dev and evade os.path.ismount."""
    if not sys.platform.startswith("linux"):
        return False
    try:
        root = path.resolve()
        for line in (
            Path("/proc/self/mountinfo")
            .read_text(encoding="utf-8", errors="surrogateescape")
            .splitlines()
        ):
            fields = line.split()
            if len(fields) < 6:
                return True
            mounted = fields[4]
            for encoded, decoded in (
                (r"\040", " "),
                (r"\011", "\t"),
                (r"\012", "\n"),
                (r"\134", "\\"),
            ):
                mounted = mounted.replace(encoded, decoded)
            if _is_relative_to(Path(mounted).resolve(), root):
                return True
        return False
    except (OSError, RuntimeError):
        return True


def _walk_size(path: Path, device: int | None = None) -> tuple[int, float] | None:
    """Get bytes and newest mtime without following, accepting no symlinks."""
    try:
        if device is None and _has_nested_mount(path):
            return None
        root_stat = os.lstat(path)
        if (
            stat.S_ISLNK(root_stat.st_mode)
            or os.path.ismount(path)
            or (device is not None and root_stat.st_dev != device)
            or not (stat.S_ISREG(root_stat.st_mode) or stat.S_ISDIR(root_stat.st_mode))
        ):
            return None
        device = root_stat.st_dev if device is None else device
        total, newest = root_stat.st_size, root_stat.st_mtime
        if not stat.S_ISDIR(root_stat.st_mode):
            return total, newest
        with os.scandir(path) as entries:
            for entry in entries:
                child = Path(entry.path)
                result = _walk_size(child, device)
                if result is None:
                    return None
                child_size, child_mtime = result
                total += child_size
                newest = max(newest, child_mtime)
        return total, newest
    except OSError:
        return None


def _valid_staging(path: Path) -> bool:
    if not path.name.startswith("codex-rich-"):
        return False
    metadata = path / "codex-package.json"
    marker = path / STAGING_MARKER
    try:
        if not (
            path.is_dir()
            and not path.is_symlink()
            and metadata.is_file()
            and not metadata.is_symlink()
            and marker.is_file()
            and not marker.is_symlink()
            and not (path / _INSTALLED_MARKER).exists()
        ):
            return False
        nofollow = getattr(os, "O_NOFOLLOW", None)
        if nofollow is None:
            return False
        descriptor = os.open(metadata, os.O_RDONLY | nofollow)
        with os.fdopen(descriptor, encoding="utf-8") as handle:
            document = json.load(handle)
        descriptor = os.open(marker, os.O_RDONLY | nofollow)
        with os.fdopen(descriptor, encoding="utf-8") as handle:
            if handle.read(128) != STAGING_MARKER_TEXT:
                return False
        if not isinstance(document, dict):
            return False
        target = document.get("target")
        variant = document.get("variant")
        if target not in _TARGETS or variant not in _VARIANT_ENTRYPOINTS:
            return False
        suffix = ".exe" if target.endswith("windows-msvc") else ""
        entrypoint = f"bin/{_VARIANT_ENTRYPOINTS[variant]}{suffix}"
        if document.get("entrypoint") != entrypoint:
            return False
        entrypoint_path = path / entrypoint
        resources = path / "codex-resources"
        path_dir = path / "codex-path"
        return (
            type(document.get("layoutVersion")) is int
            and document["layoutVersion"] == 1
            and isinstance(document.get("version"), str)
            and bool(document["version"])
            and document.get("resourcesDir") == "codex-resources"
            and document.get("pathDir") == "codex-path"
            and entrypoint_path.is_file()
            and not entrypoint_path.is_symlink()
            and resources.is_dir()
            and not resources.is_symlink()
            and path_dir.is_dir()
            and not path_dir.is_symlink()
        )
    except (OSError, UnicodeError, json.JSONDecodeError, TypeError):
        return False


def _profile_dirs(target: Path) -> list[Path]:
    result: list[Path] = []
    for base in (target, *(target / name for name in _TARGETS)):
        for profile in _PROFILES:
            path = base / profile
            try:
                if path.is_dir() and not path.is_symlink():
                    result.append(path)
            except OSError:
                continue
    return result


def _protected(path: Path, protected_paths: Iterable[Path]) -> bool:
    """Never remove a path containing, or contained by, a protected output."""
    try:
        path = path.resolve()
        if any(
            (parent / _INSTALLED_MARKER).exists() for parent in (path, *path.parents)
        ):
            return True
        if path.is_dir():

            def scan_error(error: OSError) -> None:
                raise error

            for _, _, files in os.walk(path, followlinks=False, onerror=scan_error):
                if _INSTALLED_MARKER in files:
                    return True
        return any(
            _is_relative_to(protected.resolve(), path)
            or _is_relative_to(path, protected.resolve())
            for protected in protected_paths
        )
    except (OSError, RuntimeError):
        return True


def _candidates(
    target: Path, profile: str, target_triple: str
) -> tuple[list[Candidate], int] | None:
    candidates: list[Candidate] = []
    measured_target = _walk_size(target)
    if measured_target is None:
        return None
    total, _ = measured_target
    profiles = _profile_dirs(target)
    for directory in profiles:
        measured = _walk_size(directory)
        if measured is None:
            continue
        size, mtime = measured
        current = directory in (target / profile, target / target_triple / profile)
        if current:
            # Top-level executables and other outputs survive dependency-cache eviction.
            size = 0
            for name in _CACHE_DIRS:
                child = directory / name
                if child.exists():
                    measured_child = _walk_size(child)
                    if measured_child is None:
                        return None
                    size += measured_child[0]
        # Incremental output is independently rebuildable; remove it as a whole.
        incremental = directory / "incremental"
        if incremental.exists():
            inc_measure = _walk_size(incremental)
            if inc_measure is not None:
                inc_size, inc_mtime = inc_measure
                candidates.append(
                    Candidate(incremental, "incremental", current, inc_mtime, inc_size)
                )
                size -= inc_size
        candidates.append(Candidate(directory, "profile", current, mtime, max(size, 0)))
    try:
        entries = list(os.scandir(target))
    except OSError:
        return None
    for entry in entries:
        path = Path(entry.path)
        if _valid_staging(path):
            measured = _walk_size(path)
            if measured is not None:
                size, mtime = measured
                candidates.append(Candidate(path, "staging", False, mtime, size))
    return candidates, total


def _process_snapshot() -> list[str] | None:
    """Return executable paths, never arguments that merely mention target."""
    try:
        if sys.platform == "darwin":
            import ctypes

            library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
            pidpath = library.proc_pidpath
            pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
            pidpath.restype = ctypes.c_int
            commands = []
            for value in subprocess.check_output(
                ["ps", "-axo", "pid="], text=True
            ).split():
                pid = int(value)
                if pid == os.getpid():
                    continue
                buffer = ctypes.create_string_buffer(4096)
                ctypes.set_errno(0)
                if pidpath(pid, buffer, len(buffer)) > 0:
                    commands.append(os.fsdecode(buffer.value))
                elif ctypes.get_errno() not in (errno.ENOENT, errno.ESRCH):
                    return None
            return commands
        if sys.platform.startswith("linux"):
            commands = []
            for entry in Path("/proc").iterdir():
                if not entry.name.isdigit():
                    continue
                try:
                    commands.append(os.readlink(entry / "exe"))
                except FileNotFoundError:
                    continue  # The process exited after the snapshot.
            return commands
        result = subprocess.run(
            ["ps", "-axo", "comm="],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        return result.stdout.splitlines()
    except (OSError, subprocess.SubprocessError, AttributeError, ValueError):
        return None


def _build_is_live(target: Path) -> bool | None:
    snapshot = _process_snapshot()
    if snapshot is None:
        return None
    for executable in snapshot:
        path = Path(executable.strip())
        if path.name in _WRITER_NAMES:
            return True
        if not path.is_absolute() and "target" in path.parts:
            return True  # A relative target executable may belong to another checkout.
        if not path.is_absolute():
            located = shutil.which(str(path))
            if located is None:
                continue
            path = Path(located)
        try:
            if _is_relative_to(path.resolve(), target.resolve()):
                return True
        except (OSError, RuntimeError):
            return None
    return False


def _remove(candidate: Candidate, *, dry_run: bool) -> bool:
    """Delete a whole candidate, preserving profile directories and lock inodes."""
    if _walk_size(candidate.path) is None:
        return False
    if dry_run:
        return True
    try:
        if candidate.kind == "profile":
            for child in list(candidate.path.iterdir()):
                if child.name in _LOCK_NAMES:
                    continue
                if candidate.current and child.name not in _CACHE_DIRS:
                    continue
                if (
                    _has_nested_mount(child)
                    or _walk_size(child, os.lstat(candidate.path).st_dev) is None
                ):
                    return False
                if child.is_symlink():
                    return False
                if child.is_dir():
                    shutil.rmtree(child)
                else:
                    child.unlink()
        elif candidate.path.is_dir():
            shutil.rmtree(candidate.path)
        else:
            candidate.path.unlink()
        return True
    except OSError:
        return False


def _lock_profiles(profiles: Iterable[Path]) -> tuple[list[object], str | None]:
    if os.name == "nt":
        return [], "cleanup is unsupported on Windows"
    import fcntl

    files: list[object] = []
    try:
        for profile in profiles:
            for name in _LOCK_NAMES:
                lock = profile / name
                descriptor = os.open(
                    lock, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600
                )
                if not stat.S_ISREG(os.fstat(descriptor).st_mode):
                    os.close(descriptor)
                    raise OSError("Cargo lock is not a regular file")
                handle = os.fdopen(descriptor, "a+", encoding="utf-8")
                try:
                    fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                except OSError as exc:
                    handle.close()
                    if exc.errno in (errno.EACCES, errno.EAGAIN):
                        raise BlockingIOError("Cargo artifact lock is busy") from exc
                    raise
                files.append(handle)
        return files, None
    except (OSError, BlockingIOError) as exc:
        for handle in files:
            handle.close()
        return [], f"cleanup skipped: could not acquire Cargo locks ({exc})"


def _release_locks(handles: Iterable[object]) -> None:
    for handle in handles:
        handle.close()


def _mark_profile_used(target: Path, profile: str, target_triple: str) -> None:
    """Best-effort freshness marker; failures must not make a build fail."""
    for directory in _profile_dirs(target):
        if directory in (target / profile, target / target_triple / profile):
            try:
                os.utime(directory, None)
            except OSError:
                pass


def _cleanup_after_build(
    package_dir: Path,
    profile: str,
    *,
    repo_root: Path = source_repo_root,
    dry_run: bool = False,
    protected_paths: Iterable[Path] = (),
) -> CleanupReport:
    """Best-effort cleanup limited to ``repo_root/codex-rs/target``.

    This function raises only for invalid requested limits.  Unsafe paths, Cargo
    lock collisions, and deletion failures are reported and otherwise skipped.
    """
    report = CleanupReport()
    try:
        protected = tuple(
            path.resolve() for path in (package_dir, *tuple(protected_paths))
        )
    except (OSError, RuntimeError):
        report.skipped = "cleanup skipped: could not resolve protected outputs"
        return report
    if os.environ.get("CODEX_RICH_CLEANUP") == "0":
        report.skipped = "cleanup disabled by CODEX_RICH_CLEANUP=0"
        return report
    if os.name == "nt":
        report.skipped = "cleanup is unsupported on Windows"
        return report
    if os.environ.get("CARGO_TARGET_DIR") or os.environ.get("CARGO_BUILD_BUILD_DIR"):
        report.skipped = (
            "cleanup skipped for externally configured Cargo build directories"
        )
        return report
    if profile not in _PROFILES:
        report.skipped = f"cleanup skipped: unsupported Cargo profile {profile!r}"
        return report
    target = _safe_root(repo_root)
    if target is None:
        report.skipped = "cleanup skipped: unsafe or missing codex-rs/target"
        return report
    live = _build_is_live(target)
    if live is None:
        report.skipped = "cleanup skipped: could not obtain a process snapshot"
        return report
    if live:
        report.skipped = (
            "cleanup skipped: a Cargo/Rust writer or target executable is running"
        )
        return report
    limits = read_limits()
    try:
        descriptor = os.open(
            package_dir / "codex-package.json", os.O_RDONLY | os.O_NOFOLLOW
        )
        with os.fdopen(descriptor, encoding="utf-8") as handle:
            target_triple = json.load(handle).get("target")
        if target_triple not in _TARGETS:
            raise ValueError("unsupported package target")
    except (OSError, ValueError, AttributeError):
        report.skipped = "cleanup skipped: current package target is unavailable"
        return report
    discovered = _candidates(target, profile, target_triple)
    if discovered is None:
        report.skipped = "cleanup skipped: could not scan target safely"
        return report
    candidates, total = discovered
    report.before_bytes = report.retained_bytes = total
    try:
        free = shutil.disk_usage(target).free
    except OSError:
        report.skipped = "cleanup skipped: could not measure free space"
        return report
    report.projected_free_bytes = free
    now = time.time()
    stale = [
        c
        for c in candidates
        if not c.current
        and now - c.mtime > limits.keep_seconds
        and (c.kind != "staging" or now - c.mtime > _STAGING_GRACE_SECONDS)
    ]
    # Age removals happen first.  Under pressure, mature staging goes before all
    # incremental caches; coherent profiles are a progressively colder fallback.
    ordered = sorted(stale, key=lambda c: (0 if c.kind == "staging" else 1, c.mtime))
    ordered += sorted(
        [
            c
            for c in candidates
            if c not in stale
            and c.kind == "staging"
            and now - c.mtime > _STAGING_GRACE_SECONDS
        ],
        key=lambda c: c.mtime,
    )
    ordered += sorted(
        [c for c in candidates if c not in stale and c.kind == "incremental"],
        key=lambda c: c.mtime,
    )
    ordered += sorted(
        [
            c
            for c in candidates
            if c not in stale and c.kind == "profile" and not c.current
        ],
        key=lambda c: c.mtime,
    )
    # Fresh staging packages are deliberately absent.  Current full profiles
    # are only considered after every other rebuildable cache class.
    ordered += sorted(
        [c for c in candidates if c.kind == "profile" and c.current],
        key=lambda c: c.mtime,
    )

    def needs() -> bool:
        return total > limits.max_bytes or free < limits.min_free_bytes

    if not stale and not needs():
        if not dry_run:
            _mark_profile_used(target, profile, target_triple)
        return report
    handles: list[object] = []
    if not dry_run:
        handles, error = _lock_profiles(_profile_dirs(target))
        if error:
            report.skipped = error
            return report
    try:
        if not dry_run:
            # A successful selected-profile build is useful even when it did not
            # need cleanup.  This keeps a fresh profile from ageing out later.
            _mark_profile_used(target, profile, target_triple)
        for candidate in ordered:
            if (candidate not in stale and not needs()) or _protected(
                candidate.path, protected
            ):
                continue
            if _remove(candidate, dry_run=dry_run):
                if candidate.current and candidate.kind == "profile":
                    report.cold_fallback = True
                report.deleted.append(candidate.path)
                report.bytes_freed += candidate.size
                total -= candidate.size
                if dry_run:
                    free += candidate.size
                else:
                    try:
                        free = shutil.disk_usage(target).free
                    except OSError:
                        report.unmet.append("could not remeasure free space")
                        break
            else:
                report.failed.append(candidate.path)
                report.unmet.append("candidate cleanup failed; further cleanup stopped")
                break
        if not dry_run:
            measured = _walk_size(target)
            if measured is None:
                report.unmet.append("retained cache size could not be measured")
            else:
                total = measured[0]
        report.retained_bytes = total
        if not dry_run:
            report.bytes_freed = max(0, report.before_bytes - total)
            try:
                free = shutil.disk_usage(target).free
            except OSError:
                report.unmet.append("could not remeasure free space")
        report.projected_free_bytes = free
    finally:
        _release_locks(handles)
    if total > limits.max_bytes:
        report.unmet.append("cache budget")
    if free < limits.min_free_bytes:
        report.unmet.append("minimum free-space target")
    return report


def cleanup_after_build(
    package_dir: Path,
    profile: str,
    *,
    repo_root: Path = source_repo_root,
    dry_run: bool = False,
    protected_paths: Iterable[Path] = (),
) -> CleanupReport:
    """Clean local artifacts, print its best-effort result, and return it."""
    report = _cleanup_after_build(
        package_dir,
        profile,
        repo_root=repo_root,
        dry_run=dry_run,
        protected_paths=protected_paths,
    )
    _print_report(report, dry_run=dry_run)
    return report


def _print_report(report: CleanupReport, *, dry_run: bool) -> None:
    prefix = "Would clean" if dry_run else "Cleaned"
    for path in report.deleted:
        print(f"{prefix}: {path}")
    for path in report.failed:
        print(f"Cleanup incomplete: {path}")
    if report.deleted:
        state = "removable" if dry_run else "removed"
        print(f"Cleanup cache bytes {state}: {report.bytes_freed}")
    if report.retained_bytes is not None:
        print(
            f"Cleanup cache: {report.before_bytes / 1024**3:.2f} GiB before, "
            f"{report.retained_bytes / 1024**3:.2f} GiB retained"
        )
    if report.cold_fallback:
        print("Cleanup used the current-profile cache as a cold-rebuild fallback.")
    if report.unmet:
        print(f"Cleanup could not meet: {', '.join(report.unmet)}")
    if report.skipped:
        print(report.skipped)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-dir", type=Path, required=True)
    parser.add_argument("--cargo-profile", default="dev-small")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    try:
        cleanup_after_build(args.package_dir, args.cargo_profile, dry_run=args.dry_run)
    except ValueError as exc:
        parser.error(str(exc))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
