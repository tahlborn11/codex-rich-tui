import contextlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import codex_rich_cleanup as cleanup


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


@unittest.skipIf(os.name == "nt", "Cleanup intentionally skips Windows")
class CleanupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name) / "repo"
        self.target = self.root / "codex-rs" / "target"
        self.target.mkdir(parents=True)
        self.package = self.target / "codex-rich-current"
        self.package.mkdir()
        self.package_metadata(self.package)

    def tearDown(self):
        self.temp.cleanup()

    def cache(self, relative, size=32, old=False):
        path = self.target / relative
        path.mkdir(parents=True, exist_ok=True)
        (path / "blob").write_bytes(b"x" * size)
        if old:
            then = time.time() - 20 * 86400
            os.utime(path / "blob", (then, then))
            os.utime(path, (then, then))
        return path

    def age_tree(self, path):
        then = time.time() - 20 * 86400
        for item in sorted(path.rglob("*"), reverse=True):
            os.utime(item, (then, then))
        os.utime(path, (then, then))

    def package_metadata(self, package, *, target="aarch64-apple-darwin", staging=True):
        entrypoint = package / "bin" / "codex"
        entrypoint.parent.mkdir(exist_ok=True)
        entrypoint.write_bytes(b"codex")
        (package / "codex-resources").mkdir(exist_ok=True)
        (package / "codex-path").mkdir(exist_ok=True)
        (package / "codex-package.json").write_text(
            json.dumps(
                {
                    "layoutVersion": 1,
                    "version": "1.2.3",
                    "target": target,
                    "variant": "codex",
                    "entrypoint": entrypoint.relative_to(package).as_posix(),
                    "resourcesDir": "codex-resources",
                    "pathDir": "codex-path",
                }
            )
        )
        if staging:
            (package / ".codex-rich-staging").write_text("codex-rich-staging-v1\n")

    def cleanup(self, profile="dev-small", dry_run=False, protected_paths=(), **env):
        defaults = dict(
            CODEX_RICH_CACHE_MAX_GIB="999",
            CODEX_RICH_MIN_FREE_GIB="0",
            CODEX_RICH_CACHE_KEEP_DAYS="14",
            CARGO_TARGET_DIR=None,
            CARGO_BUILD_BUILD_DIR=None,
            CODEX_RICH_CLEANUP=None,
        )
        defaults.update(env)
        with environment(**defaults):
            with patch.object(cleanup, "_process_snapshot", return_value=[]):
                return cleanup.cleanup_after_build(
                    self.package,
                    profile,
                    repo_root=self.root,
                    dry_run=dry_run,
                    protected_paths=protected_paths,
                )

    def test_warm_current_profile_is_retained(self):
        current = self.cache("dev-small/incremental/current")
        report = self.cleanup()
        self.assertTrue(current.exists())
        self.assertFalse(report.deleted)

    def test_stale_inactive_profile_and_staging_are_removed(self):
        old_profile = self.cache("release", old=True)
        old_stage = self.cache("codex-rich-old")
        self.package_metadata(old_stage)
        self.age_tree(old_stage)
        report = self.cleanup()
        self.assertFalse((old_profile / "blob").exists())
        self.assertFalse(old_stage.exists())
        self.assertIn(old_stage, report.deleted)

    def test_current_package_and_nested_content_are_protected(self):
        protected = self.cache("dev-small/deps/keep/nested", old=True)
        self.package = self.target / "dev-small" / "deps" / "keep"
        self.package_metadata(self.package)
        report = self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertIsNone(report.skipped)
        self.assertTrue(protected.exists())
        self.assertNotIn(self.target / "dev-small", report.deleted)

    def test_requested_archive_inside_profile_is_protected(self):
        profile = self.cache("release", old=True)
        archive = profile / "release.tar.gz"
        archive.write_bytes(b"archive")
        with environment(
            CODEX_RICH_CACHE_MAX_GIB="0",
            CODEX_RICH_MIN_FREE_GIB="0",
            CODEX_RICH_CACHE_KEEP_DAYS="14",
            CARGO_TARGET_DIR=None,
            CARGO_BUILD_BUILD_DIR=None,
            CODEX_RICH_CLEANUP=None,
        ):
            with patch.object(cleanup, "_process_snapshot", return_value=[]):
                cleanup.cleanup_after_build(
                    self.package,
                    "dev-small",
                    repo_root=self.root,
                    protected_paths=(archive,),
                )
        self.assertTrue(archive.exists())
        self.assertTrue((profile / "blob").exists())

    def test_budget_prefers_incremental_then_uses_cold_fallback(self):
        other_incremental = self.cache("release/incremental/old", size=128)
        retained = self.cache("release/deps", size=64)
        current_incremental = self.cache("dev-small/incremental/current", size=16384)
        report = self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0.00001")
        self.assertFalse(other_incremental.parent.exists())
        self.assertFalse(current_incremental.parent.exists())
        self.assertTrue(retained.exists())
        self.assertFalse(report.cold_fallback)

    def test_current_full_profile_is_last_cold_fallback(self):
        current_deps = self.cache("dev-small/deps", size=128)
        profile = self.target / "dev-small"
        (profile / "codex").write_bytes(b"entrypoint")
        (profile / "codex-code-mode-host").write_bytes(b"host")
        report = self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertFalse(current_deps.exists())
        self.assertTrue((profile / "codex").exists())
        self.assertTrue((profile / "codex-code-mode-host").exists())
        self.assertTrue(report.cold_fallback)

    def test_external_dirs_and_unsafe_symlinks_skip(self):
        item = self.cache("release", old=True)
        self.cleanup(CARGO_TARGET_DIR="/elsewhere", CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue(item.exists())
        outside = Path(self.temp.name) / "outside"
        outside.mkdir()
        (self.root / "codex-rs").rename(outside / "codex-rs")
        (self.root / "codex-rs").symlink_to(
            outside / "codex-rs", target_is_directory=True
        )
        report = self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertIsNotNone(report.skipped)

    def test_unknown_profile_skips_all_deletion(self):
        item = self.cache("release", old=True)
        report = self.cleanup("unrecognized", CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertIsNotNone(report.skipped)
        self.assertTrue(item.exists())

    def test_fresh_staging_is_retained_even_under_pressure(self):
        stage = self.cache("codex-rich-fresh", size=128)
        self.package_metadata(stage)
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue(stage.exists())

    def test_mature_staging_is_pruned_under_pressure(self):
        stage = self.cache("codex-rich-mature", size=128)
        self.package_metadata(stage)
        then = time.time() - 2 * 86400
        for item in stage.rglob("*"):
            os.utime(item, (then, then))
        os.utime(stage, (then, then))
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertFalse(stage.exists())

    def test_unmarked_canonical_package_is_never_staging(self):
        package = self.cache("codex-rich-unmarked")
        self.package_metadata(package, staging=False)
        self.age_tree(package)
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue(package.exists())

    def test_installed_staging_marker_is_protected_under_pressure(self):
        stage = self.cache("codex-rich-installed", size=128)
        self.package_metadata(stage)
        (stage / ".codex-rich-installed").write_text("installed\n")
        self.age_tree(stage)
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue(stage.exists())

    def test_installed_marker_in_profile_is_protected_under_pressure(self):
        profile = self.cache("release/deps", size=128).parent
        (profile / "deps" / ".codex-rich-installed").write_text("installed\n")
        self.age_tree(profile)
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue((profile / "deps").exists())

    def test_protected_output_with_dotdot_is_preserved(self):
        profile = self.cache("release", old=True)
        archive = profile / "release.tar.gz"
        archive.write_bytes(b"archive")
        dotted_archive = profile / "nested" / ".." / "release.tar.gz"
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0", protected_paths=(dotted_archive,))
        self.assertTrue(archive.exists())

    @unittest.skipUnless(sys.platform == "darwin", "macOS path alias regression")
    def test_protected_output_with_private_var_alias_is_preserved(self):
        if not str(self.temp.name).startswith("/var/"):
            self.skipTest("temporary directory is not under /var")
        profile = self.cache("release", old=True)
        archive = profile / "release.tar.gz"
        archive.write_bytes(b"archive")
        alias = Path("/private/var") / Path(self.temp.name).relative_to("/var") / "repo"
        alias_archive = alias / "codex-rs" / "target" / "release" / "release.tar.gz"
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0", protected_paths=(alias_archive,))
        self.assertTrue(archive.exists())

    def test_nested_symlink_makes_candidate_unsafe(self):
        profile = self.cache("release", old=True)
        (profile / "link").symlink_to(Path(self.temp.name))
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue(profile.exists())

    def test_invalid_and_unknown_target_staging_are_preserved(self):
        malformed = self.cache("codex-rich-malformed", old=True)
        (malformed / "codex-package.json").write_text("{}")
        unknown = self.cache("codex-rich-unknown", old=True)
        self.package_metadata(unknown, target="made-up-target")
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue(malformed.exists())
        self.assertTrue(unknown.exists())

    def test_unknown_target_profile_folder_is_preserved(self):
        unknown = self.cache("made-up-target/release", old=True)
        report = self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue((unknown / "blob").exists())
        self.assertIn("cache budget", report.unmet)

    def test_wrong_target_profile_is_evicted_while_selected_target_is_kept(self):
        selected = self.cache("aarch64-apple-darwin/dev-small/deps", old=True)
        wrong = self.cache("x86_64-apple-darwin/dev-small/deps", old=True)
        self.age_tree(wrong.parent)
        self.cleanup()
        self.assertTrue(selected.exists())
        self.assertFalse(wrong.exists())

    def test_linux_bind_mount_is_rejected_even_without_device_change(self):
        mount = str((self.target / "release" / "bind").resolve()).replace(" ", r"\040")
        with (
            patch.object(cleanup.sys, "platform", "linux"),
            patch.object(
                Path,
                "read_text",
                return_value=f"36 35 8:1 / {mount} rw - ext4 /dev/root rw\n",
            ),
        ):
            self.assertTrue(cleanup._has_nested_mount(self.target))
        with (
            patch.object(cleanup.sys, "platform", "linux"),
            patch.object(
                Path, "read_text", side_effect=OSError("unreadable mount table")
            ),
        ):
            self.assertTrue(cleanup._has_nested_mount(self.target))

    def test_mount_boundary_skips_all_deletion(self):
        protected = self.cache("release/deps", old=True)
        later = self.cache("ci-test/deps", old=True)
        with patch.object(
            cleanup.os.path,
            "ismount",
            side_effect=lambda path: Path(path) == protected,
        ):
            report = self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertIsNotNone(report.skipped)
        self.assertTrue(protected.exists())
        self.assertTrue(later.exists())

    def test_partial_clear_failure_stops_later_candidates_and_reports_unmet(self):
        release_deps = self.cache("release/deps", old=True)
        release_build = self.cache("release/build", old=True)
        later = self.cache("ci-test/deps", old=True)
        real_rmtree = shutil.rmtree

        def fail_build(path, *args, **kwargs):
            if Path(path) == release_build:
                raise OSError("simulated partial cleanup failure")
            return real_rmtree(path, *args, **kwargs)

        with patch.object(cleanup.shutil, "rmtree", side_effect=fail_build):
            report = self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertFalse(release_deps.exists())
        self.assertTrue(release_build.exists())
        self.assertTrue(later.exists())
        self.assertTrue(report.unmet or getattr(report, "errors", []))

    def test_process_snapshot_failure_and_writer_skip(self):
        item = self.cache("release", old=True)
        with patch.object(cleanup, "_process_snapshot", return_value=None):
            report = cleanup.cleanup_after_build(
                self.package, "dev-small", repo_root=self.root
            )
        self.assertIsNotNone(report.skipped)
        with patch.object(
            cleanup, "_process_snapshot", return_value=["/usr/bin/cargo"]
        ):
            report = cleanup.cleanup_after_build(
                self.package, "dev-small", repo_root=self.root
            )
        self.assertIsNotNone(report.skipped)
        self.assertTrue(item.exists())

    def test_free_space_pressure_is_best_effort(self):
        profile = self.cache("release", size=128)
        with patch.object(
            cleanup.shutil, "disk_usage", return_value=type("Disk", (), {"free": 0})()
        ):
            report = self.cleanup(CODEX_RICH_MIN_FREE_GIB="1")
        self.assertFalse((profile / "blob").exists())
        self.assertIn("minimum free-space target", report.unmet)

    def test_current_profile_is_marked_recently_used(self):
        profile = self.cache("dev-small", old=True)
        before = profile.stat().st_mtime
        self.cleanup()
        self.assertGreater(profile.stat().st_mtime, before)

    @unittest.skipIf(
        os.name == "nt", "POSIX locks are intentionally unsupported on Windows"
    )
    def test_busy_lock_skips_all_deletion(self):
        profile = self.cache("release", old=True)
        lock = profile / ".cargo-lock"
        code = (
            "import fcntl,sys,time; f=open(sys.argv[1],'a+'); "
            "fcntl.flock(f,fcntl.LOCK_EX); print('locked',flush=True); time.sleep(10)"
        )
        process = subprocess.Popen(
            [sys.executable, "-c", code, str(lock)],
            stdout=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertEqual(process.stdout.readline().strip(), "locked")
            report = self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
            self.assertIsNotNone(report.skipped)
            self.assertTrue((profile / "blob").exists())
        finally:
            process.terminate()
            process.wait()
            process.stdout.close()

    def test_profile_lock_inode_is_preserved(self):
        profile = self.cache("release", old=True)
        lock = profile / ".cargo-lock"
        lock.write_text("lock")
        inode = lock.stat().st_ino
        self.cleanup(CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue(lock.exists())
        self.assertEqual(inode, lock.stat().st_ino)

    def test_dry_run_does_not_mutate_or_create_locks(self):
        profile = self.cache("release", old=True)
        report = self.cleanup(dry_run=True, CODEX_RICH_CACHE_MAX_GIB="0")
        self.assertTrue(report.deleted)
        self.assertTrue((profile / "blob").exists())
        self.assertFalse((profile / ".cargo-lock").exists())

    def test_malformed_limits_fail(self):
        with self.assertRaises(ValueError):
            self.cleanup(CODEX_RICH_CACHE_MAX_GIB="nan")
        with self.assertRaises(ValueError):
            self.cleanup(CODEX_RICH_CACHE_KEEP_DAYS="-1")


class UnsupportedPlatformTests(unittest.TestCase):
    def test_windows_is_explicitly_unsupported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            item = root / "keep"
            item.write_text("preserve")
            with (
                environment(CODEX_RICH_CLEANUP=None),
                patch.object(cleanup.os, "name", "nt"),
            ):
                report = cleanup.cleanup_after_build(root, "dev-small", repo_root=root)
            self.assertIn("Windows", report.skipped)
            self.assertEqual(item.read_text(), "preserve")


@unittest.skipIf(os.name == "nt", "POSIX process inspection is unsupported on Windows")
class PosixProcessSnapshotTests(unittest.TestCase):
    @unittest.skipUnless(Path("/bin/sleep").is_file(), "requires /bin/sleep")
    def test_snapshot_reports_actual_executable_path_not_argument_text(self):
        # Copying a signed system binary can cause macOS to kill the copy before
        # inspection. Keep the executable intact and put a decoy path in argv.
        decoy = "/nonexistent/codex-cache-test/target/rustc"
        process = subprocess.Popen([decoy, "20"], executable="/bin/sleep")
        try:
            expected = str(Path("/bin/sleep").resolve())
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                snapshot = cleanup._process_snapshot()
                if snapshot is not None and expected in snapshot:
                    break
                time.sleep(0.05)
            self.assertIsNotNone(snapshot)
            self.assertTrue(
                expected in snapshot,
                "snapshot did not include the launched executable",
            )
            self.assertNotIn(decoy, snapshot)
        finally:
            process.terminate()
            process.wait()


if __name__ == "__main__":
    unittest.main()
