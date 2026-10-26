import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

if os.name != "nt":
    import fcntl

sys.path.insert(0, str(Path(__file__).parent))
import codex_rich_cache as cache


@contextlib.contextmanager
def environment(**values):
    old = {key: os.environ.get(key) for key in values}
    try:
        for key, value in values.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = str(value)
        yield
    finally:
        for key, value in old.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


@unittest.skipIf(os.name == "nt", "local cache is intentionally unsupported on Windows")
class CacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name) / "repo"
        self.target = self.root / "codex-rs" / "target"
        self.target.mkdir(parents=True)
        self.store = Path(self.temp.name) / "store"
        self.args = SimpleNamespace(
            cargo="cargo",
            cargo_profile="dev-small",
            target="aarch64-apple-darwin",
            archive_output=[],
        )
        self.identity = (
            {"schema": 1, "rustc": "r", "cargo": "c"},
            {"schema": 1, "rustc": "r", "cargo": "c", "lock": "one"},
        )

    def tearDown(self):
        self.temp.cleanup()

    def populate(self):
        path = self.target / "dev-small" / "deps" / "nested" / "artifact"
        path.parent.mkdir(parents=True)
        path.write_bytes(b"cache data")
        os.chmod(path, 0o640)
        os.utime(path, ns=(1_700_000_000_123_456_789, 1_700_000_000_987_654_321))
        (self.target / "dev-small" / "codex").write_bytes(b"output")
        return path

    def session(self):
        return cache.local_build_cache(self.args, repo_root=self.root)

    def limits(self):
        return environment(CODEX_RICH_MIN_FREE_GIB="0", CODEX_RICH_CACHE_MAX_GIB="999")

    def patches(self):
        return (
            patch.object(cache, "_repo_cache_root", return_value=self.store),
            patch.object(cache, "_identity", return_value=self.identity),
            patch.object(cache, "_build_is_live", return_value=False),
        )

    def malicious_archive(self, member_name, *, kind):
        """Create a small `.tar.zst` without using cache implementation."""
        raw = self.store / "payload.tar"
        archive = self.store / f"{hash(member_name)}.tar.zst"
        self.store.mkdir(exist_ok=True)
        with tarfile.open(raw, "w") as document:
            item = tarfile.TarInfo(member_name)
            if kind == "symlink":
                item.type = tarfile.SYMTYPE
                item.linkname = "../../outside"
            elif kind == "special":
                item.type = tarfile.CHRTYPE
            else:
                item.size = 1
                document.addfile(item, fileobj=io.BytesIO(b"x"))
                item = None
            if item is not None:
                document.addfile(item)
        subprocess.run(
            [shutil.which("zstd") or "zstd", "-q", "-f", str(raw), "-o", str(archive)],
            check=True,
        )
        raw.unlink()
        return archive

    def selection_archive(self, name, hard, exact, *, scope=None):
        self.store.mkdir(exist_ok=True)
        archive = self.store / f"{name}.tar.zst"
        archive.write_bytes(name.encode())
        cache._manifest_path(archive).write_text(
            json.dumps(
                {
                    "schema": 2,
                    "hard": hard,
                    "exact": exact,
                    "members": ["dev-small/deps"],
                    "scopes": [scope or "aarch64-apple-darwin/dev-small"],
                    "expandedBytes": 0,
                    "archiveBytes": archive.stat().st_size,
                    "archiveSha256": cache._file_hash(archive),
                }
            )
        )
        return archive

    @unittest.skipUnless(
        shutil.which("bsdtar") and shutil.which("zstd"), "archive tools unavailable"
    )
    def test_round_trip_preserves_mode_and_nanoseconds(self):
        source = self.populate()
        expected = (
            source.read_bytes(),
            stat.S_IMODE(source.stat().st_mode),
            source.stat().st_mtime_ns,
        )
        package = self.root / "package"
        package.mkdir()
        with self.limits(), self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session() as lifecycle:
                lifecycle.save(package)
            self.assertFalse(source.exists())
            self.assertTrue((self.target / "dev-small" / "codex").exists())
            with self.session() as lifecycle:
                self.assertTrue(lifecycle.restored)
        restored = self.target / "dev-small" / "deps" / "nested" / "artifact"
        self.assertEqual(
            (
                restored.read_bytes(),
                stat.S_IMODE(restored.stat().st_mode),
                restored.stat().st_mtime_ns,
            ),
            expected,
        )

    def test_warm_cache_is_never_overwritten(self):
        self.populate()
        with self.limits(), self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session() as lifecycle:
                self.assertFalse(lifecycle.restored)
        self.assertTrue(
            (self.target / "dev-small" / "deps" / "nested" / "artifact").exists()
        )

    @unittest.skipUnless(
        shutil.which("bsdtar") and shutil.which("zstd"), "archive tools unavailable"
    )
    def test_cold_symlinked_profile_never_restores_outside_target(self):
        self.populate()
        package = self.root / "package"
        package.mkdir()
        with self.limits(), self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session() as lifecycle:
                lifecycle.save(package)
        shutil.rmtree(self.target / "dev-small")
        outside = Path(self.temp.name) / "outside"
        outside.mkdir()
        (self.target / "dev-small").symlink_to(outside, target_is_directory=True)
        with self.limits(), self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session() as lifecycle:
                self.assertFalse(lifecycle.restored)
        self.assertFalse((outside / "deps").exists())

    def test_exact_and_compatible_selection(self):
        hard, exact = self.identity
        exact_archive = self.selection_archive("exact", hard, exact)
        fallback = self.selection_archive("fallback", hard, dict(exact, lock="other"))
        session = cache.CacheSession(
            self.args, repo_root=self.root, cache_root=self.store
        )
        self.assertEqual(session._select(hard, exact), exact_archive)
        self.assertEqual(session._select(hard, dict(exact, lock="new")), fallback)
        self.assertIsNone(session._select(dict(hard, rustc="different"), exact))

    def test_retention_only_prunes_managed_pairs_and_preserves_current_snapshot(self):
        old = self.selection_archive("old", *self.identity)
        current = self.selection_archive("current", *self.identity)
        os.utime(old, (1, 1))
        orphan = self.store / "orphan.tar.zst"
        orphan.write_bytes(b"manual archive")
        unrelated = self.store / "manual.zip"
        unrelated.write_bytes(b"manual archive")
        link = self.store / "linked.tar.zst"
        link.symlink_to(orphan)
        cache._prune_managed(self.store, current, 0, 0)
        self.assertFalse(old.exists())
        self.assertFalse(cache._manifest_path(old).exists())
        self.assertIsNotNone(cache._archive_record(current))
        self.assertTrue(orphan.exists())
        self.assertTrue(unrelated.exists())
        self.assertTrue(link.is_symlink())

    def test_malicious_or_corrupt_archive_is_rejected(self):
        archive = self.store / "bad.tar.zst"
        self.store.mkdir()
        archive.write_bytes(b"not an archive")
        self.assertFalse(
            cache._validate_archive(archive, shutil.which("zstd") or "zstd")
        )
        self.assertFalse(cache._valid_name("../escape"))
        self.assertFalse(cache._valid_name("release/link"))

    @unittest.skipUnless(shutil.which("zstd"), "zstd unavailable")
    def test_malicious_tar_members_never_restore_or_touch_sentinel(self):
        outside = Path(self.temp.name) / "outside"
        outside.mkdir()
        sentinel = outside / "sentinel"
        sentinel.write_text("safe")
        package = self.root / "package"
        package.mkdir()
        for name, kind in (
            ("../escape", "file"),
            ("/absolute", "file"),
            ("dev-small/deps/link", "symlink"),
            ("dev-small/deps/device", "special"),
        ):
            with self.subTest(name=name):
                archive = self.malicious_archive(name, kind=kind)
                cache._manifest_path(archive).write_text(
                    json.dumps(
                        {
                            "schema": 2,
                            "hard": self.identity[0],
                            "exact": self.identity[1],
                            "members": ["dev-small/deps"],
                            "scopes": ["dev-small"],
                            "expandedBytes": 1,
                            "archiveBytes": archive.stat().st_size,
                            "archiveSha256": cache._file_hash(archive),
                        }
                    )
                )
                self.assertFalse(
                    cache._restore(archive, self.target, cache._archive_tools(), 0)
                )
                self.assertEqual(sentinel.read_text(), "safe")

    def test_disabled_and_custom_build_directory_skip(self):
        self.populate()
        with (
            environment(CODEX_RICH_CACHE="0"),
            self.patches()[0],
            self.patches()[1],
            self.patches()[2],
        ):
            with self.session() as lifecycle:
                self.assertFalse(lifecycle.enabled)
        with (
            environment(CARGO_TARGET_DIR="/tmp/custom"),
            self.patches()[0],
            self.patches()[1],
            self.patches()[2],
        ):
            with self.session() as lifecycle:
                self.assertFalse(lifecycle.enabled)

    def test_failed_build_context_does_not_snapshot(self):
        self.populate()
        package = self.root / "package"
        package.mkdir()
        with self.limits(), self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session():
                pass
        self.assertFalse(list(self.store.glob("*.tar.zst")))

    def test_cleanup_zero_keeps_expanded_cache_after_snapshot(self):
        source = self.populate()
        package = self.root / "package"
        package.mkdir()
        with (
            self.limits(),
            environment(CODEX_RICH_CLEANUP="0"),
            self.patches()[0],
            self.patches()[1],
            self.patches()[2],
        ):
            with self.session() as lifecycle:
                lifecycle.save(package)
        self.assertTrue(source.exists())
        self.assertTrue(list(self.store.glob("*.tar.zst")))

    @unittest.skipUnless(
        shutil.which("bsdtar") and shutil.which("zstd"), "archive tools unavailable"
    )
    def test_restore_releases_profile_locks_before_yielding(self):
        self.populate()
        package = self.root / "package"
        package.mkdir()
        with self.limits(), self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session() as lifecycle:
                lifecycle.save(package)
            with self.session() as lifecycle:
                self.assertTrue(lifecycle.restored)
                lock = self.target / "dev-small" / ".cargo-lock"
                with lock.open("a+") as handle:
                    fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                    fcntl.flock(handle.fileno(), fcntl.LOCK_UN)

    def test_low_reserve_and_protected_output_keep_expanded_cache(self):
        source = self.populate()
        package = self.root / "package"
        package.mkdir()
        with (
            self.limits(),
            self.patches()[0],
            self.patches()[1],
            self.patches()[2],
            patch.object(cache, "_free", return_value=False),
        ):
            with self.session() as lifecycle:
                lifecycle.save(package)
        self.assertTrue(source.exists())
        self.assertFalse(list(self.store.glob("*.tar.zst")))

    def test_busy_or_unknown_process_probe_prevents_snapshot_and_eviction(self):
        source = self.populate()
        package = self.root / "package"
        package.mkdir()
        with (
            self.limits(),
            patch.object(cache, "_repo_cache_root", return_value=self.store),
            patch.object(cache, "_identity", return_value=self.identity),
            patch.object(cache, "_build_is_live", side_effect=[False, False, True]),
        ):
            with self.session() as lifecycle:
                self.assertTrue(lifecycle.enabled)
                lifecycle.save(package)
        self.assertTrue(source.exists())
        self.assertFalse(list(self.store.glob("*.tar.zst")))
        with (
            self.limits(),
            patch.object(cache, "_repo_cache_root", return_value=self.store),
            patch.object(cache, "_identity", return_value=self.identity),
            patch.object(cache, "_build_is_live", return_value=None),
        ):
            with self.session() as lifecycle:
                self.assertFalse(lifecycle.enabled)
        protected_package = self.target / "dev-small" / "deps" / "package"
        protected_package.mkdir(parents=True)
        with self.limits(), self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session() as lifecycle:
                lifecycle.save(protected_package)
        self.assertTrue(source.exists())
        self.assertFalse(list(self.store.glob("*.tar.zst")))

    @unittest.skipUnless(
        shutil.which("bsdtar") and shutil.which("zstd"), "archive tools unavailable"
    )
    def test_partial_restore_rolls_back_moved_directories(self):
        self.populate()
        build = self.target / "dev-small" / "build" / "output"
        build.parent.mkdir(parents=True)
        build.write_bytes(b"build cache")
        package = self.root / "package"
        package.mkdir()
        with self.limits(), self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session() as lifecycle:
                lifecycle.save(package)
        archive = next(self.store.glob("*.tar.zst"))
        replace = os.replace

        def fail_second_move(source, destination):
            if Path(destination) == self.target / "dev-small" / "build":
                raise OSError("injected move failure")
            return replace(source, destination)

        with (
            self.limits(),
            patch.object(cache, "_build_is_live", return_value=False),
            patch.object(cache.os, "replace", side_effect=fail_second_move),
        ):
            self.assertFalse(
                cache._restore(archive, self.target, cache._archive_tools(), 0)
            )
        self.assertFalse((self.target / "dev-small" / "deps").exists())
        self.assertFalse((self.target / "dev-small" / "build").exists())

    @unittest.skipUnless(
        shutil.which("bsdtar") and shutil.which("zstd"), "archive tools unavailable"
    )
    def test_same_identity_refreshes_snapshot_and_failed_publish_keeps_previous(self):
        source = self.populate()
        package = self.root / "package"
        package.mkdir()
        with (
            self.limits(),
            environment(CODEX_RICH_CLEANUP="0"),
            self.patches()[0],
            self.patches()[1],
            self.patches()[2],
        ):
            with self.session() as lifecycle:
                lifecycle.save(package)
            first = next(self.store.glob("*.tar.zst"))
            source.write_bytes(b"changed cache data")
            with self.session() as lifecycle:
                lifecycle.save(package)
            snapshots = list(self.store.glob("*.tar.zst"))
            self.assertEqual(len(snapshots), 2)
            second = next(path for path in snapshots if path != first)
            self.assertNotEqual(cache._file_hash(first), cache._file_hash(second))
            with patch.object(cache, "_validate_archive", return_value=False):
                with self.session() as lifecycle:
                    lifecycle.save(package)
        self.assertEqual(len(list(self.store.glob("*.tar.zst"))), 2)
        self.assertTrue(source.exists())

    @unittest.skipUnless(cache._archive_tools(), "archive tools unavailable")
    def test_failed_durable_publication_preserves_expanded_cache(self):
        source = self.populate()
        package = self.root / "package"
        package.mkdir()
        with (
            self.limits(),
            self.patches()[0],
            self.patches()[1],
            self.patches()[2],
            patch.object(
                cache.os, "fsync", side_effect=OSError("injected sync failure")
            ),
        ):
            with self.session() as lifecycle:
                lifecycle.save(package)
        self.assertTrue(source.exists())
        self.assertFalse(list(self.store.glob("*.tar.zst")))

    def test_workspace_build_dir_config_skips_and_hard_identity_tracks_flags_profiles(
        self,
    ):
        workspace = self.root / "codex-rs"
        manifest = workspace / "Cargo.toml"
        manifest.write_text("[workspace]\n[profile.dev]\ndebug = 1\n")
        (workspace / "Cargo.lock").write_text("lock-one\n")
        config = workspace / ".cargo" / "config.toml"
        config.parent.mkdir()
        config.write_text("[build]\ntarget-dir = 'elsewhere'\n")
        with self.patches()[0], self.patches()[1], self.patches()[2]:
            with self.session() as lifecycle:
                self.assertFalse(lifecycle.enabled)
        config.unlink()
        with (
            patch.object(cache, "_command_version", return_value="toolchain"),
            patch.object(cache, "_config_files", return_value=[]),
            environment(RUSTFLAGS="-Cdebuginfo=1"),
        ):
            first_hard, first_exact = cache._identity(self.args, self.root)
            manifest.write_text("[workspace]\n[profile.dev]\ndebug = 2\n")
            second_hard, second_exact = cache._identity(self.args, self.root)
            self.assertNotEqual(first_hard, second_hard)
            manifest.write_text("[workspace]\n[profile.dev]\ndebug = 2\n")
            (workspace / "Cargo.lock").write_text("lock-two\n")
            third_hard, third_exact = cache._identity(self.args, self.root)
        self.assertEqual(second_hard, third_hard)
        self.assertNotEqual(second_exact, third_exact)

    def test_new_member_after_profile_locks_skips_snapshot_and_eviction(self):
        source = self.populate()
        package = self.root / "package"
        package.mkdir()
        session = cache.CacheSession(
            self.args,
            repo_root=self.root,
            target=self.target,
            cache_root=self.store,
            enabled=True,
        )
        members = [Path("dev-small/deps")]
        changed_members = [*members, Path("release/deps")]
        with (
            patch.object(cache, "_build_is_live", return_value=False),
            patch.object(
                cache, "_archive_members", side_effect=[members, changed_members]
            ),
            patch.object(cache, "_lock_profiles", return_value=([], None)),
            patch.object(session, "_save_locked") as save,
        ):
            session.save(package)
        save.assert_not_called()
        self.assertTrue(source.exists())

    def test_post_flock_identity_is_used_for_selection(self):
        old = ({"hard": "old"}, {"exact": "old"})
        new = ({"hard": "new"}, {"exact": "new"})
        with (
            patch.object(cache, "_repo_cache_root", return_value=self.store),
            patch.object(cache, "_identity", side_effect=[old, new]),
            patch.object(cache, "_build_is_live", return_value=False),
            patch.object(cache, "_archive_tools", return_value=("tar", "zstd")),
            patch.object(cache.CacheSession, "_select", return_value=None) as select,
        ):
            with self.session() as lifecycle:
                self.assertTrue(lifecycle.enabled)
        select.assert_called_once_with(*new)

    @unittest.skipUnless(shutil.which("zstd"), "zstd unavailable")
    def test_manifest_member_dot_is_rejected_before_restore_mutates_target(self):
        archive = self.malicious_archive("dev-small/deps/artifact", kind="file")
        cache._manifest_path(archive).write_text(
            json.dumps(
                {
                    "schema": 2,
                    "hard": self.identity[0],
                    "exact": self.identity[1],
                    "members": ["."],
                    "scopes": ["dev-small"],
                    "expandedBytes": 1,
                    "archiveBytes": archive.stat().st_size,
                    "archiveSha256": cache._file_hash(archive),
                }
            )
        )
        self.assertFalse(
            cache._restore(archive, self.target, cache._archive_tools(), 0)
        )
        self.assertFalse((self.target / "dev-small").exists())


if __name__ == "__main__":
    unittest.main()
