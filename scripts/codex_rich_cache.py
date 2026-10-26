#!/usr/bin/env python3
"""Restore and archive local Cargo artifacts without discarding an unverified cache."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
from typing import Iterable
import uuid

from codex_rich_cleanup import (
    _CACHE_DIRS,
    _PROFILES,
    _TARGETS,
    _build_is_live,
    _lock_profiles,
    _profile_dirs,
    _protected,
    _release_locks,
    _safe_root,
    _walk_size,
    read_limits,
    source_repo_root,
)

_SCHEMA = 2
_MAX_MANIFEST = 1024 * 1024
_MAX_MEMBERS = 1_000_000
_ENV = {
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC",
    "RUSTDOC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_INCREMENTAL",
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CC",
    "CXX",
    "AR",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "SDKROOT",
    "MACOSX_DEPLOYMENT_TARGET",
    "V8_FROM_SOURCE",
    "RUSTY_V8_ARCHIVE",
    "RUSTY_V8_SRC_BINDING_PATH",
    "CARGO_HOME",
}


def _log(message: str) -> None:
    print(f"Codex Rich build cache: {message}", flush=True)


def _sha(parts: Iterable[bytes]) -> str:
    digest = hashlib.sha256()
    for part in parts:
        digest.update(part)
        digest.update(b"\0")
    return digest.hexdigest()


def _file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(4 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _key(document: dict[str, object]) -> str:
    return _sha([json.dumps(document, sort_keys=True, separators=(",", ":")).encode()])


def _command_version(
    command: str, flag: str = "-vV", *, cwd: Path | None = None
) -> str | None:
    try:
        return subprocess.check_output(
            [command, flag], text=True, stderr=subprocess.DEVNULL, cwd=cwd
        ).strip()
    except (OSError, subprocess.SubprocessError):
        return None


def _repo_cache_root(repo_root: Path) -> Path:
    canonical = str(repo_root.resolve()).encode("utf-8", "surrogateescape")
    return (
        Path.home()
        / ".cache"
        / "codex-rich-tui"
        / "build-cache"
        / _sha([canonical])[:24]
    )


def _config_files(repo_root: Path) -> list[Path]:
    workspace = repo_root / "codex-rs"
    bases = [workspace, *workspace.parents]
    cargo_home = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
    return list(
        dict.fromkeys(
            [
                *(
                    base / ".cargo" / name
                    for base in bases
                    for name in ("config", "config.toml")
                ),
                *(cargo_home / name for name in ("config", "config.toml")),
            ]
        )
    )


def _safe_target(args: object, repo_root: Path) -> Path | None:
    if any(
        name in os.environ
        for name in (
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
            "CARGO_BUILD_TARGET_DIR",
        )
    ):
        return None
    for config in _config_files(repo_root):
        if config.exists() and re.search(
            r"\b(?:build-dir|target-dir)\b", config.read_text(encoding="utf-8")
        ):
            return None
    # Keep custom/prebuilt packaging out of automatic Cargo cache eviction.
    if (
        getattr(args, "entrypoint_bin", None) is not None
        and getattr(args, "code_mode_host_bin", None) is not None
    ):
        return None
    workspace = repo_root / "codex-rs"
    if repo_root.is_symlink() or workspace.is_symlink() or not workspace.is_dir():
        return None
    target = workspace / "target"
    if target.is_symlink():
        return None
    target.mkdir(exist_ok=True)
    return _safe_root(repo_root)


def _profile_configuration(manifest: Path) -> str:
    # A conservative fallback also works on Python 3.10 without tomllib.
    lines: list[str] = []
    collecting = False
    for line in manifest.read_text(encoding="utf-8").splitlines():
        if line.lstrip().startswith("["):
            collecting = bool(re.match(r"\s*\[profile(?:\.|\])", line))
        if collecting:
            lines.append(line)
    return _sha(["\n".join(lines).encode()])


def _identity(
    args: object, repo_root: Path
) -> tuple[dict[str, object], dict[str, object]] | None:
    workspace = repo_root / "codex-rs"
    cargo = str(getattr(args, "cargo", "cargo"))
    cargo_version = _command_version(cargo, cwd=workspace)
    rustc_version = _command_version(os.environ.get("RUSTC") or "rustc", cwd=workspace)
    if (
        not cargo_version
        or not rustc_version
        or getattr(args, "cargo_profile", "dev-small") not in _PROFILES
    ):
        return None
    # Whitelist compilation inputs: CARGO_REGISTRIES_* may contain credentials.
    environment = {
        name: value
        for name, value in os.environ.items()
        if name in _ENV
        or name.startswith(
            (
                "CARGO_PROFILE_",
                "CARGO_TARGET_",
                "CFLAGS_",
                "CXXFLAGS_",
                "CPPFLAGS_",
                "LDFLAGS_",
                "CC_",
                "CXX_",
                "AR_",
            )
        )
    }
    environment.pop("CARGO_TARGET_DIR", None)
    configs = {
        str(path): _file_hash(path)
        for path in _config_files(repo_root)
        if path.is_file()
    }
    manifests: dict[str, str] = {}
    for current, dirs, files in os.walk(workspace, followlinks=False):
        dirs[:] = sorted(
            name
            for name in dirs
            if name not in {"target", ".git"}
            and not (Path(current) / name).is_symlink()
        )
        if "Cargo.toml" in files:
            path = Path(current) / "Cargo.toml"
            manifests[str(path.relative_to(workspace))] = _file_hash(path)
    toolchains = {
        str(path): _file_hash(path)
        for base in (repo_root, workspace)
        for name in ("rust-toolchain", "rust-toolchain.toml")
        if (path := base / name).is_file()
    }
    hard = {
        "schema": _SCHEMA,
        "cargo": cargo_version,
        "rustc": rustc_version,
        "system": sys.platform,
        "machine": platform.machine(),
        "configuration": _key(configs),
        "profiles": _profile_configuration(workspace / "Cargo.toml"),
        "environment": _key(environment),
        "toolchains": toolchains,
    }
    lock = workspace / "Cargo.lock"
    exact = {
        **hard,
        "lock": _file_hash(lock) if lock.exists() else "missing",
        "manifests": manifests,
    }
    return hard, exact


def _restore_parents_safe(target: Path) -> bool:
    try:
        for base in (target, *(target / triple for triple in _TARGETS)):
            if base.is_symlink() or (
                base.exists() and (not base.is_dir() or os.path.ismount(base))
            ):
                return False
            for profile in _PROFILES:
                path = base / profile
                if path.is_symlink() or (
                    path.exists() and (not path.is_dir() or os.path.ismount(path))
                ):
                    return False
        return True
    except OSError:
        return False


def _known_paths(target: Path) -> list[Path]:
    return [
        profile / name
        for profile in _profile_dirs(target)
        for name in sorted(_CACHE_DIRS)
        if (profile / name).exists() or (profile / name).is_symlink()
    ]


def _cold(target: Path) -> bool:
    return not _known_paths(target)


def _archive_members(target: Path) -> list[Path] | None:
    if not _restore_parents_safe(target):
        return None
    members = []
    for item in _known_paths(target):
        if not item.is_dir() or _walk_size(item) is None:
            return None
        members.append(item.relative_to(target))
    return members


def _archive_tools() -> tuple[str, str] | None:
    tar, zstd = shutil.which("bsdtar") or shutil.which("tar"), shutil.which("zstd")
    return (tar, zstd) if tar and zstd else None


def _free(path: Path, required: int) -> bool:
    return shutil.disk_usage(path).free >= required


def _manifest_path(archive: Path) -> Path:
    return archive.with_suffix(archive.suffix + ".json")


def _load_manifest(path: Path) -> dict[str, object] | None:
    try:
        if (
            path.is_symlink()
            or not stat.S_ISREG(path.stat().st_mode)
            or path.stat().st_size > _MAX_MANIFEST
        ):
            return None
        value = json.loads(path.read_text(encoding="utf-8"))
        return value if isinstance(value, dict) else None
    except (OSError, ValueError, UnicodeError):
        return None


def _valid_name(name: str) -> bool:
    path = PurePosixPath(name)
    parts = path.parts
    if path.is_absolute() or ".." in parts or not parts or len(name) > 8192:
        return False
    if parts[0] in _PROFILES:
        return len(parts) == 1 or parts[1] in _CACHE_DIRS
    return parts[0] in _TARGETS and (
        len(parts) == 1
        or (parts[1] in _PROFILES and (len(parts) == 2 or parts[2] in _CACHE_DIRS))
    )


def _validate_archive(
    archive: Path, zstd: str, expanded_limit: int | None = None
) -> bool:
    """Read all payload/checksums and reject unsafe paths and file types."""
    process = None
    try:
        process = subprocess.Popen(
            [zstd, "-q", "-d", "-c", str(archive)],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        assert process.stdout is not None
        seen: set[str] = set()
        regular: set[str] = set()
        total = 0
        with tarfile.open(fileobj=process.stdout, mode="r|") as tar:
            for member in tar:
                normalized = str(PurePosixPath(member.name))
                if (
                    not _valid_name(member.name)
                    or normalized in seen
                    or len(seen) >= _MAX_MEMBERS
                ):
                    return False
                parts = PurePosixPath(member.name).parts
                boundary = 1 if parts[0] in _PROFILES else 2
                if len(parts) <= boundary + 1 and not member.isdir():
                    return False
                if member.islnk():
                    if (
                        not _valid_name(member.linkname)
                        or str(PurePosixPath(member.linkname)) not in regular
                    ):
                        return False
                    regular.add(normalized)
                elif member.isreg():
                    total += member.size
                    if member.size < 0 or (
                        expanded_limit is not None and total > expanded_limit
                    ):
                        return False
                    regular.add(normalized)
                elif not member.isdir():
                    return False
                seen.add(normalized)
        # tar stops at its end marker before the compressor's final checksum.
        # Bound trailing padding instead of waiting on a producer with a full pipe.
        if len(process.stdout.read(1024 * 1024 + 1)) > 1024 * 1024:
            return False
        return bool(seen) and process.wait() == 0
    except (OSError, tarfile.TarError, ValueError):
        return False
    finally:
        if process is not None:
            if process.stdout is not None:
                process.stdout.close()
            if process.poll() is None:
                process.kill()
            process.wait()


def _archive_record(
    archive: Path, *, verify_digest: bool = True
) -> dict[str, object] | None:
    document = _load_manifest(_manifest_path(archive))
    try:
        if (
            not document
            or document.get("schema") != _SCHEMA
            or archive.is_symlink()
            or not stat.S_ISREG(archive.stat().st_mode)
        ):
            return None
        if (
            archive.stat().st_size != document["archiveBytes"]
            or not isinstance(document["expandedBytes"], int)
            or document["expandedBytes"] < 0
            or not isinstance(document.get("members"), list)
            or not isinstance(document.get("scopes"), list)
            or not isinstance(document.get("hard"), dict)
            or not isinstance(document.get("exact"), dict)
        ):
            return None
        if verify_digest and _file_hash(archive) != document["archiveSha256"]:
            return None
        return document
    except (OSError, KeyError, TypeError):
        return None


def _restore(archive: Path, target: Path, tools: tuple[str, str], reserve: int) -> bool:
    tar, zstd = tools
    document = _archive_record(archive)
    if not document or not _cold(target) or not _restore_parents_safe(target):
        return False
    size = int(document["expandedBytes"])
    if not _free(target, reserve + size) or not _validate_archive(archive, zstd, size):
        return False
    names = document["members"]
    if not all(isinstance(name, str) for name in names):
        return False
    members = [Path(name) for name in names]
    if not members or len(set(members)) != len(members):
        return False
    for path in members:
        if not path.parts or not _valid_name(str(path)):
            return False
        expected_depth = 2 if path.parts[0] in _PROFILES else 3
        if (
            not _valid_name(str(path))
            or path.name not in _CACHE_DIRS
            or len(path.parts) != expected_depth
        ):
            return False
    for relative in members:
        (target / relative).parent.mkdir(parents=True, exist_ok=True)
    locks, error = _lock_profiles(_profile_dirs(target))
    if error:
        return False
    try:
        if (
            _build_is_live(target) is not False
            or not _cold(target)
            or not _restore_parents_safe(target)
        ):
            return False
        with tempfile.TemporaryDirectory(
            prefix="codex-rich-restore-", dir=target.parent
        ) as temporary:
            staging = Path(temporary)
            producer = consumer = None
            try:
                producer = subprocess.Popen(
                    [zstd, "-q", "-d", "-c", str(archive)],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                )
                assert producer.stdout is not None
                consumer = subprocess.Popen(
                    [tar, "-xpf", "-", "-C", str(staging)],
                    stdin=producer.stdout,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                producer.stdout.close()
                consumer_code = consumer.wait()
                producer_code = producer.wait()
                if consumer_code or producer_code:
                    raise OSError("decompression pipeline failed")
            finally:
                for process in (consumer, producer):
                    if process is not None:
                        if process.poll() is None:
                            process.kill()
                        process.wait()
            extracted = _archive_members(staging)
            if (
                extracted is None
                or set(extracted) != set(members)
                or not _cold(target)
                or not _restore_parents_safe(target)
            ):
                return False
            moved: list[Path] = []
            try:
                for relative in extracted:
                    destination = target / relative
                    if destination.exists() or destination.is_symlink():
                        raise OSError("cache destination appeared during restore")
                    os.replace(staging / relative, destination)
                    moved.append(relative)
            except OSError:
                for relative in reversed(moved):
                    os.replace(target / relative, staging / relative)
                raise
            return True
    except (OSError, subprocess.SubprocessError) as error:
        _log(f"restore skipped ({error}); Cargo will rebuild")
        return False
    finally:
        _release_locks(locks)


def _evict(
    target: Path, protected: Iterable[Path], members: list[Path] | None = None
) -> bool:
    actual = _archive_members(target)
    if actual is None or (members is not None and set(actual) != set(members)):
        return False
    if any(_protected(target / relative, protected) for relative in actual):
        return False
    for relative in actual:
        shutil.rmtree(target / relative)
    return True


def _prune_managed(
    cache_root: Path, keep: Path, max_bytes: int, keep_seconds: float
) -> None:
    # Do not touch manually stored files, symlinks, orphan archives, or unknown schemas.
    managed = [
        (path.stat().st_mtime, path, path.stat().st_size)
        for path in cache_root.glob("*.tar.zst")
        if _archive_record(path, verify_digest=False) is not None
    ]
    total = sum(size for _, _, size in managed)
    current = next((item for item in managed if item[1] == keep), None)
    for mtime, archive, size in sorted(managed):
        if archive == keep or (
            total <= max_bytes and time.time() - mtime <= keep_seconds
        ):
            continue
        archive.unlink()
        _manifest_path(archive).unlink()
        total -= size
    if current and total > max_bytes:
        _log("archive budget exceeded; keeping the verified current snapshot")


@dataclass
class CacheSession:
    args: object
    repo_root: Path = source_repo_root
    target: Path | None = None
    cache_root: Path | None = None
    lifecycle_lock: object | None = None
    enabled: bool = False
    restored: bool = False

    def _select(self, hard: dict[str, object], exact: dict[str, object]) -> Path | None:
        assert self.cache_root is not None
        target = getattr(self.args, "target", None)
        if not target:
            from codex_package.targets import default_target

            target = default_target()
        profile = str(getattr(self.args, "cargo_profile", "dev-small"))
        profile = "debug" if profile == "dev" else profile
        scope = f"{target}/{profile}"
        candidates = []
        for path in self.cache_root.glob("*.tar.zst"):
            document = _archive_record(path, verify_digest=False)
            if document and document.get("hard") == hard:
                candidates.append(
                    (
                        scope not in document.get("scopes", []),
                        document.get("exact") != exact,
                        -path.stat().st_mtime,
                        path,
                    )
                )
        for _, _, _, path in sorted(candidates):
            if _archive_record(path) is not None:
                return path
        return None

    def __enter__(self) -> "CacheSession":
        if os.environ.get("CODEX_RICH_CACHE") == "0" or os.name == "nt":
            return self
        try:
            self.target = _safe_target(self.args, self.repo_root)
            if self.target is None:
                _log("skipped for an external, prebuilt, or unsafe build directory")
                return self
            if _build_is_live(self.target) is not False:
                _log("busy or unavailable process check; keeping expanded artifacts")
                return self
            tools = _archive_tools()
            identity = _identity(self.args, self.repo_root)
            if tools is None or identity is None:
                _log(
                    "tar/zstd or toolchain information unavailable; keeping expanded artifacts"
                )
                return self
            self.cache_root = _repo_cache_root(self.repo_root)
            if self.cache_root.is_symlink():
                return self
            self.cache_root.mkdir(mode=0o700, parents=True, exist_ok=True)
            import fcntl

            descriptor = os.open(
                self.cache_root / ".lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600
            )
            self.lifecycle_lock = os.fdopen(descriptor, "a+")
            fcntl.flock(self.lifecycle_lock.fileno(), fcntl.LOCK_EX)
            # Settings and the toolchain may change while another package build
            # holds the lifecycle lock. Never select using the pre-wait identity.
            self.target = _safe_target(self.args, self.repo_root)
            tools = _archive_tools()
            identity = _identity(self.args, self.repo_root)
            if (
                self.target is None
                or tools is None
                or identity is None
                or _build_is_live(self.target) is not False
            ):
                return self
            self.enabled = True
            if _cold(self.target):
                archive = self._select(*identity)
                if archive:
                    self.restored = _restore(
                        archive, self.target, tools, read_limits().min_free_bytes
                    )
                    _log(
                        f"{'restored' if self.restored else 'restore skipped'}: {archive.name}"
                    )
                else:
                    _log("no compatible archive; Cargo will build from source")
            else:
                _log("using existing expanded artifacts")
        except Exception as error:
            self.enabled = False
            _log(f"skipped ({error}); keeping expanded artifacts")
        return self

    def save(self, package_dir: Path) -> None:
        if not self.enabled or self.target is None or self.cache_root is None:
            return
        try:
            if _build_is_live(self.target) is not False:
                _log("build process active; snapshot and eviction skipped")
                return
            members = _archive_members(self.target)
            if not members:
                return
            locks, error = _lock_profiles(
                sorted({(self.target / path).parent for path in members})
            )
            if error:
                _log(error)
                return
            try:
                # A direct Cargo invocation can add a profile while locks are
                # acquired. Archive only the fixed, locked set, or skip entirely.
                if set(_archive_members(self.target) or []) != set(members):
                    return
                self._save_locked(package_dir, members)
            finally:
                _release_locks(locks)
        except Exception as error:
            _log(f"snapshot skipped ({error}); keeping expanded artifacts")

    def _save_locked(self, package_dir: Path, members: list[Path]) -> None:
        assert self.target is not None and self.cache_root is not None
        if _build_is_live(self.target) is not False:
            return
        tools, identity = (
            _archive_tools(),
            _identity(self.args, self.repo_root),
        )
        if not tools or not identity or not members:
            _log("no safe Cargo cache directories to archive")
            return
        protected = [
            package_dir.resolve(),
            *(
                path.resolve()
                for path in (getattr(self.args, "archive_output", None) or [])
            ),
        ]
        if any(_protected(self.target / path, protected) for path in members):
            _log("protected output inside Cargo cache; snapshot and eviction skipped")
            return
        measured = 0
        for path in members:
            size = _walk_size(self.target / path)
            if size is None:
                return
            measured += size[0]
        limits = read_limits()
        if not _free(self.cache_root, limits.min_free_bytes + measured):
            _log("insufficient archive staging space; keeping expanded artifacts")
            return
        tar, zstd = tools
        hard, exact = identity
        archive = self.cache_root / f"{_key(exact)}-{uuid.uuid4().hex}.tar.zst"
        with tempfile.TemporaryDirectory(
            prefix="snapshot-", dir=self.cache_root
        ) as temporary:
            compressed = Path(temporary) / "cache.tar.zst"
            environment = {**os.environ, "COPYFILE_DISABLE": "1"}
            _log(f"compressing {measured / 1024**3:.2f} GiB of Cargo artifacts")
            producer = consumer = None
            try:
                producer = subprocess.Popen(
                    [
                        tar,
                        "--format=pax",
                        "-cf",
                        "-",
                        "-C",
                        str(self.target),
                        *map(str, members),
                    ],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    env=environment,
                )
                assert producer.stdout is not None
                consumer = subprocess.Popen(
                    [zstd, "-q", "-3", "-T4", "-o", str(compressed)],
                    stdin=producer.stdout,
                    stderr=subprocess.DEVNULL,
                )
                producer.stdout.close()
                consumer_code = consumer.wait()
                producer_code = producer.wait()
                if consumer_code or producer_code:
                    raise OSError("compression pipeline failed")
            finally:
                for process in (consumer, producer):
                    if process is not None:
                        if process.poll() is None:
                            process.kill()
                        process.wait()
            if not _validate_archive(compressed, zstd, measured):
                raise OSError("archive validation failed")
            document = {
                "schema": _SCHEMA,
                "hard": hard,
                "exact": exact,
                "members": list(map(str, members)),
                "scopes": sorted({str(path.parent) for path in members}),
                "expandedBytes": measured,
                "archiveBytes": compressed.stat().st_size,
                "archiveSha256": _file_hash(compressed),
                "created": time.time(),
            }
            manifest = Path(temporary) / "manifest.json"
            manifest.write_text(
                json.dumps(document, sort_keys=True) + "\n", encoding="utf-8"
            )
            for path in (compressed, manifest):
                with path.open("rb") as stream:
                    os.fsync(stream.fileno())
            # Immutable archive first, manifest last. Interrupted publication cannot
            # make an older manifest describe new bytes or replace the last good pair.
            os.replace(compressed, archive)
            os.replace(manifest, _manifest_path(archive))
            descriptor = os.open(self.cache_root, os.O_RDONLY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        _log(
            f"snapshot verified: {archive.stat().st_size / 1024**3:.2f} GiB ({archive.name})"
        )
        if os.environ.get("CODEX_RICH_CLEANUP") != "0":
            if _evict(self.target, protected, members):
                _log("expanded Cargo artifacts removed; package outputs preserved")
        _prune_managed(self.cache_root, archive, limits.max_bytes, limits.keep_seconds)

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> None:
        if self.lifecycle_lock is not None:
            self.lifecycle_lock.close()


def local_build_cache(
    args: object, *, repo_root: Path = source_repo_root
) -> CacheSession:
    return CacheSession(args=args, repo_root=repo_root)
