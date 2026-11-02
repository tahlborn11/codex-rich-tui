#!/usr/bin/env python3
"""Integration checks for the signed local Rich package installer."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
INSTALLER = REPOSITORY_ROOT / "scripts" / "install_codex_rich_local.sh"
IDENTIFIERS = [
    "com.tahlborn.codex-rich",
    "com.tahlborn.codex-rich.code-mode-host",
    "com.tahlborn.codex-rich.exec-server",
]


class InstallCodexRichLocalTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.fake_bin = self.root / "fake-bin"
        self.fake_bin.mkdir()
        self.sign_log = self.root / "signatures.log"
        self._write_fake_commands()

    def test_installs_and_signs_all_package_binaries_before_atomic_replacement(
        self,
    ) -> None:
        package = self._package()
        install = self.root / "installed"
        launcher = self.root / "launcher"

        result = self._run(package, install, launcher)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.sign_log.read_text().splitlines(), IDENTIFIERS)
        for binary in ("codex", "codex-code-mode-host", "codex-exec-server"):
            self.assertEqual((install / "bin" / binary).read_text(), f"{binary}\n")
        self.assertEqual(
            (install / "codex-package.json").read_text(), '{"package": true}\n'
        )
        self.assertEqual(
            (install / "resources" / "managed.txt").read_text(), "managed\n"
        )
        self.assertEqual(
            os.readlink(launcher / "codex-rich"),
            str(install.resolve() / "bin" / "codex"),
        )

    def test_missing_or_unsigned_exec_server_preserves_existing_install(self) -> None:
        package = self._package()
        install = self.root / "installed"
        launcher = self.root / "launcher"
        for binary in ("codex", "codex-code-mode-host", "codex-exec-server"):
            target = install / "bin" / binary
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(f"old-{binary}\n")

        (package / "bin" / "codex-exec-server").unlink()
        missing = self._run(package, install, launcher)
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("missing package binary", missing.stderr)
        self._assert_existing_install(install)

        exec_server = package / "bin" / "codex-exec-server"
        exec_server.write_text("codex-exec-server\n")
        exec_server.chmod(0o755)
        failed_sign = self._run(
            package,
            install,
            launcher,
            {"FAKE_CODESIGN_FAIL_IDENTIFIER": IDENTIFIERS[-1]},
        )
        self.assertNotEqual(failed_sign.returncode, 0)
        self._assert_existing_install(install)

    def _assert_existing_install(self, install: Path) -> None:
        for binary in ("codex", "codex-code-mode-host", "codex-exec-server"):
            self.assertEqual((install / "bin" / binary).read_text(), f"old-{binary}\n")

    def _package(self) -> Path:
        package = self.root / "package"
        (package / "bin").mkdir(parents=True)
        (package / "resources").mkdir()
        (package / "codex-package.json").write_text('{"package": true}\n')
        (package / "resources" / "managed.txt").write_text("managed\n")
        for binary in ("codex", "codex-code-mode-host", "codex-exec-server"):
            path = package / "bin" / binary
            path.write_text(f"{binary}\n")
            path.chmod(0o755)
        return package

    def _run(
        self,
        package: Path,
        install: Path,
        launcher: Path,
        extra_environment: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{self.fake_bin}:{environment['PATH']}",
                "CODEX_RICH_BIN_DIR": str(launcher),
                "FAKE_CODESIGN_LOG": str(self.sign_log),
            }
        )
        if extra_environment:
            environment.update(extra_environment)
        return subprocess.run(
            ["zsh", str(INSTALLER), str(package), str(install)],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    def _write_fake_commands(self) -> None:
        self._command("uname", "#!/bin/zsh\nprintf 'Darwin\\n'\n")
        self._command(
            "security",
            "#!/bin/zsh\nprintf '  1) ABCDEF \"Codex Rich Local Signing\"\\n'\n",
        )
        self._command(
            "codesign",
            """#!/bin/zsh
if [[ "$1" == "-d" ]]; then
  target="${@[-1]}"
  identifier="$(awk -F '|' -v target="$target" '$2 == target { print $1 }' "$FAKE_CODESIGN_LOG.state")"
  print -u2 'designated => identifier "'$identifier'"'
  print -u2 'certificate root = H"abc"'
  exit 0
fi
identifier=''
for (( index = 1; index <= $#; index++ )); do
  if [[ "${@[index]}" == '--identifier' ]]; then
    identifier="${@[$((index + 1))]}"
  fi
done
if [[ "$identifier" == "${FAKE_CODESIGN_FAIL_IDENTIFIER:-}" ]]; then
  print -u2 'signing failed'
  exit 1
fi
print -r -- "$identifier" >> "$FAKE_CODESIGN_LOG"
print -r -- "$identifier|${@[-1]}" >> "$FAKE_CODESIGN_LOG.state"
""",
        )

    def _command(self, name: str, source: str) -> None:
        command = self.fake_bin / name
        command.write_text(source)
        command.chmod(0o755)


if __name__ == "__main__":
    unittest.main()
