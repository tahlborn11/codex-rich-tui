//! Resolves the Apple-signed Codex CLI used by the Rich macOS frontend.
//!
//! Rich keeps its TUI in this process. Credential-mutating subcommands run in
//! the official CLI because macOS Keychain partitions credentials by signer.

use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;

const DESKTOP_CLI: &str =
    "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex";
const OFFICIAL_OVERRIDE: &str = "CODEX_RICH_OFFICIAL_CLI";
const OPENAI_REQUIREMENT: &str = "identifier \"codex\" and anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"";

pub(crate) fn resolve_official_cli(self_executable: &Path) -> anyhow::Result<PathBuf> {
    let override_path = std::env::var_os(OFFICIAL_OVERRIDE).map(PathBuf::from);
    let override_supplied = override_path.is_some();
    let candidates = match override_path {
        Some(path) => vec![path],
        None => default_candidates(),
    };
    resolve_candidates(
        candidates,
        override_supplied,
        self_executable,
        &SystemVerifier,
    )
}

fn resolve_candidates(
    candidates: impl IntoIterator<Item = PathBuf>,
    override_supplied: bool,
    self_executable: &Path,
    verifier: &impl CandidateVerifier,
) -> anyhow::Result<PathBuf> {
    let mut failures = Vec::new();
    for candidate in candidates {
        match validate_candidate(&candidate, self_executable, verifier) {
            Ok(path) => return Ok(path),
            Err(error) if override_supplied => {
                return Err(error.context(format!(
                    "{OFFICIAL_OVERRIDE} must name a compatible Apple-signed Codex CLI"
                )));
            }
            Err(error) => failures.push(format!("{}: {error}", candidate.display())),
        }
    }
    anyhow::bail!(
        "Rich could not find a compatible Apple-signed Codex backend. Update the ChatGPT desktop app or install the official Codex CLI. Checked: {}",
        failures.join("; ")
    )
}

pub(crate) fn exec_official_cli(
    official: &Path,
    args: impl IntoIterator<Item = OsString>,
    remote_control_disabled: bool,
) -> anyhow::Result<()> {
    let mut command = official_command(official, args, remote_control_disabled);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        Err(anyhow::Error::from(error).context(format!(
            "failed to start official Codex backend {}",
            official.display()
        )))
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn official_command(
    official: &Path,
    args: impl IntoIterator<Item = OsString>,
    remote_control_disabled: bool,
) -> Command {
    let mut command = Command::new(official);
    command.args(args);
    // main consumes this internal marker before starting Tokio. Restore it
    // only in the replacement process so the official backend keeps the same
    // remote-control restriction.
    if remote_control_disabled {
        command.env("CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED", "1");
    }
    command
}

/// Ensures that the local daemon selected for Rich is the same trusted official
/// package. A stopped official selection may be started. An existing Rich
/// selection is never replaced automatically.
pub(crate) fn prepare_local_daemon(official: &Path) -> anyhow::Result<PathBuf> {
    let official_version = SystemVerifier.version(official)?;
    // Even a read-only lifecycle probe can inspect the selected executable's
    // version. Authenticate that selection before invoking the probe.
    validate_passive_selected_daemon(official, &official_version)?;
    let mut daemon = daemon_version(official).ok();
    if daemon
        .as_ref()
        .is_none_or(|daemon| daemon.status != "running")
    {
        let output = Command::new(official)
            .args(["app-server", "daemon", "start"])
            .output()
            .context("could not start the official local app server")?;
        anyhow::ensure!(
            output.status.success(),
            "official local app server did not start: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        daemon = Some(daemon_version(official)?);
    }
    let daemon =
        daemon.ok_or_else(|| anyhow::anyhow!("official daemon did not report its version"))?;
    anyhow::ensure!(
        daemon.status == "running",
        "the official app server is not running; run `{}` app-server daemon update --from-cli` after finishing active work",
        official.display()
    );
    validate_selected_daemon(&daemon, official, &official_version)?;
    let socket = daemon
        .socket_path
        .ok_or_else(|| anyhow::anyhow!("official daemon did not report a socket path"))?;
    anyhow::ensure!(
        socket.is_absolute(),
        "official daemon reported a relative socket path"
    );
    Ok(socket)
}

/// Refuse lifecycle commands that would start or restart a non-official
/// selected package. A fresh install has no selected path and is safe to start
/// through the already validated official CLI.
pub(crate) fn ensure_daemon_lifecycle_target(official: &Path) -> anyhow::Result<()> {
    let official_version = SystemVerifier.version(official)?;
    validate_passive_selected_daemon(official, &official_version)
}

/// A stopped daemon can reject `daemon version` before it emits JSON. Its
/// managed `current` symlink is still safe to inspect without starting it.
fn validate_passive_selected_daemon(official: &Path, official_version: &str) -> anyhow::Result<()> {
    let selected = managed_daemon_cli_path();
    if let Some(selected) = selected {
        let selected =
            validate_candidate(&selected, Path::new("/__codex_rich__"), &SystemVerifier)?;
        let selected_version = SystemVerifier.version(&selected)?;
        anyhow::ensure!(
            selected_version.trim() == official_version.trim(),
            "the selected daemon package differs from the official backend; run `{}` app-server daemon update --from-cli` after finishing active work",
            official.display()
        );
    }
    Ok(())
}

fn managed_daemon_cli_path() -> Option<PathBuf> {
    let path = codex_app_server_daemon::selected_codex_path().ok()?;
    existing_selection_path(path)
}

fn existing_selection_path(path: PathBuf) -> Option<PathBuf> {
    // Keep broken selections visible so validation rejects them instead of
    // treating a damaged or legacy installation as a fresh home.
    let parent = path.parent()?;
    let selection = if parent.ends_with("bin") {
        parent.parent()?
    } else {
        parent
    };
    (!matches!(selection.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound))
        .then_some(path)
}

fn validate_selected_daemon(
    daemon: &DaemonVersion,
    official: &Path,
    official_version: &str,
) -> anyhow::Result<()> {
    validate_running_identity(daemon, official_version)?;
    let managed = daemon.managed_codex_path.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "the local daemon has no official package selected; run `{}` app-server daemon update --from-cli` after finishing active work",
            official.display()
        )
    })?;
    let managed = validate_candidate(managed, Path::new("/__codex_rich__"), &SystemVerifier)?;
    let managed_version = SystemVerifier.version(&managed)?;
    anyhow::ensure!(
        managed_version.trim() == official_version.trim(),
        "the selected daemon package differs from the official backend; run `{}` app-server daemon update --from-cli` after finishing active work",
        official.display()
    );
    Ok(())
}

fn validate_running_identity(daemon: &DaemonVersion, official_version: &str) -> anyhow::Result<()> {
    if daemon.status == "running" {
        anyhow::ensure!(
            daemon.backend.as_deref() == Some("pid"),
            "Codex Rich requires the managed official app server, not an unmanaged socket owner"
        );
        anyhow::ensure!(
            daemon.app_server_version.as_deref()
                == official_version.trim().strip_prefix("codex-cli "),
            "the running daemon differs from the selected official package; finish active work, then restart the official daemon"
        );
    }
    Ok(())
}

struct DaemonVersion {
    status: String,
    backend: Option<String>,
    managed_codex_path: Option<PathBuf>,
    socket_path: Option<PathBuf>,
    app_server_version: Option<String>,
}

fn daemon_version(official: &Path) -> anyhow::Result<DaemonVersion> {
    let output = Command::new(official)
        .args(["app-server", "daemon", "version"])
        .output()
        .context("could not inspect the local app server")?;
    anyhow::ensure!(
        output.status.success(),
        "could not inspect the local app server"
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("official daemon returned invalid JSON")?;
    let field = |name| value.get(name).and_then(serde_json::Value::as_str);
    Ok(DaemonVersion {
        status: field("status").unwrap_or_default().to_string(),
        backend: field("backend").map(str::to_string),
        managed_codex_path: field("managedCodexPath").map(PathBuf::from),
        socket_path: field("socketPath").map(PathBuf::from),
        app_server_version: field("appServerVersion").map(str::to_string),
    })
}

fn default_candidates() -> Vec<PathBuf> {
    let daemon = managed_daemon_cli_path();
    let local = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin/codex"));
    daemon
        .into_iter()
        .chain(Some(PathBuf::from(DESKTOP_CLI)))
        .chain(local)
        .collect()
}

trait CandidateVerifier {
    fn verify_signature(&self, candidate: &Path) -> anyhow::Result<()>;
    fn version(&self, candidate: &Path) -> anyhow::Result<String>;
}

struct SystemVerifier;

impl CandidateVerifier for SystemVerifier {
    fn verify_signature(&self, candidate: &Path) -> anyhow::Result<()> {
        let output = Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", &format!("-R={OPENAI_REQUIREMENT}")])
            .arg(candidate)
            .output()
            .context("could not run codesign")?;
        anyhow::ensure!(
            output.status.success(),
            "signature does not satisfy the OpenAI requirement"
        );
        Ok(())
    }

    fn version(&self, candidate: &Path) -> anyhow::Result<String> {
        let output = Command::new(candidate)
            .arg("--version")
            .output()
            .context("could not read official CLI version")?;
        anyhow::ensure!(
            output.status.success(),
            "official CLI version command failed"
        );
        String::from_utf8(output.stdout).context("official CLI version was not UTF-8")
    }
}

fn validate_candidate(
    candidate: &Path,
    self_executable: &Path,
    verifier: &impl CandidateVerifier,
) -> anyhow::Result<PathBuf> {
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("candidate does not exist: {}", candidate.display()))?;
    let canonical = native_executable(&canonical);
    anyhow::ensure!(canonical.is_file(), "candidate is not a file");
    let self_canonical = self_executable
        .canonicalize()
        .unwrap_or_else(|_| self_executable.into());
    anyhow::ensure!(
        canonical != self_canonical,
        "candidate resolves to Rich itself"
    );
    verifier.verify_signature(&canonical)?;
    let version = verifier.version(&canonical)?;
    anyhow::ensure!(
        is_compatible_version(&version),
        "incompatible official CLI version {version:?}"
    );
    Ok(canonical)
}

/// Desktop packages expose `bin/codex` as a launcher script. Credential work
/// must use its signed native executable, not the script.
fn native_executable(candidate: &Path) -> PathBuf {
    let Some(package_root) = candidate.parent().and_then(Path::parent) else {
        return candidate.into();
    };
    if candidate
        .parent()
        .is_some_and(|parent| parent.ends_with("bin"))
        && candidate.file_name().is_some_and(|name| name == "codex")
    {
        let native = package_root.join("CodexCLI.app/Contents/MacOS/codex");
        if native.is_file() {
            return native;
        }
    }
    candidate.into()
}

fn is_compatible_version(version: &str) -> bool {
    let Some(version) = version.trim().strip_prefix("codex-cli ") else {
        return false;
    };
    version_family(version) == version_family(env!("CARGO_PKG_VERSION"))
        && version_family(version).is_some()
}

fn version_family(version: &str) -> Option<(u64, u64)> {
    let base = version.split(['-', '+']).next()?;
    let mut parts = base.split('.');
    let family = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    let _: u64 = parts.next()?.parse().ok()?;
    (parts.next().is_none()).then_some(family)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tempfile::NamedTempFile;

    struct Verifier {
        verified: Cell<u8>,
        versioned: Cell<u8>,
        signature_ok: bool,
        version: &'static str,
    }

    impl CandidateVerifier for Verifier {
        fn verify_signature(&self, _: &Path) -> anyhow::Result<()> {
            self.verified.set(self.verified.get() + 1);
            anyhow::ensure!(self.signature_ok, "rejected signature");
            Ok(())
        }
        fn version(&self, _: &Path) -> anyhow::Result<String> {
            self.versioned.set(self.versioned.get() + 1);
            Ok(self.version.into())
        }
    }

    #[test]
    fn accepts_the_current_official_version_family() {
        assert!(is_compatible_version("codex-cli 0.167.0-alpha.5.1\n"));
        assert!(is_compatible_version("codex-cli 0.167.9"));
        assert!(!is_compatible_version("codex-rich 2026.11.03"));
        assert!(!is_compatible_version("codex-cli 0.164.0"));
        assert!(!is_compatible_version("codex-cli 0.166.0"));
        assert!(!is_compatible_version("codex-cli 0.167.invalid"));
        assert!(!is_compatible_version("codex-cli 0.167.0.1"));
    }

    #[test]
    fn rejected_signature_is_never_probed_for_a_version() {
        let candidate = NamedTempFile::new().unwrap();
        let self_executable = NamedTempFile::new().unwrap();
        let verifier = Verifier {
            verified: Cell::new(0),
            versioned: Cell::new(0),
            signature_ok: false,
            version: "codex-cli 0.167.0",
        };
        assert!(validate_candidate(candidate.path(), self_executable.path(), &verifier).is_err());
        assert_eq!(verifier.verified.get(), 1);
        assert_eq!(verifier.versioned.get(), 0);
    }

    #[test]
    fn invalid_override_fails_closed_without_trying_a_fallback() {
        let override_candidate = NamedTempFile::new().unwrap();
        let fallback_candidate = NamedTempFile::new().unwrap();
        let self_executable = NamedTempFile::new().unwrap();
        let verifier = Verifier {
            verified: Cell::new(0),
            versioned: Cell::new(0),
            signature_ok: false,
            version: "codex-cli 0.167.0",
        };
        assert!(
            resolve_candidates(
                vec![
                    override_candidate.path().into(),
                    fallback_candidate.path().into(),
                ],
                true,
                self_executable.path(),
                &verifier,
            )
            .is_err()
        );
        assert_eq!(verifier.verified.get(), 1);
        assert_eq!(verifier.versioned.get(), 0);
    }

    #[test]
    fn rejects_rich_self_before_signature_or_version_checks() {
        let executable = NamedTempFile::new().unwrap();
        let verifier = Verifier {
            verified: Cell::new(0),
            versioned: Cell::new(0),
            signature_ok: true,
            version: "codex-cli 0.167.0",
        };
        assert!(validate_candidate(executable.path(), executable.path(), &verifier).is_err());
        assert_eq!(verifier.verified.get(), 0);
        assert_eq!(verifier.versioned.get(), 0);
    }

    #[test]
    fn broken_selection_is_not_treated_as_a_fresh_home() {
        let home = tempfile::tempdir().unwrap();
        let selected = home.path().join("current/bin/codex");
        assert!(existing_selection_path(selected.clone()).is_none());
        std::fs::create_dir(home.path().join("current")).unwrap();
        assert_eq!(existing_selection_path(selected.clone()), Some(selected));
    }

    #[test]
    fn desktop_launcher_resolves_to_the_signed_native_bundle() {
        let root = tempfile::tempdir().unwrap();
        let launcher = root.path().join("bin/codex");
        let native = root.path().join("CodexCLI.app/Contents/MacOS/codex");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(&launcher, "#!/bin/sh\n").unwrap();
        std::fs::write(&native, "native fixture").unwrap();
        assert_eq!(native_executable(&launcher), native);
    }

    #[test]
    fn running_identity_requires_a_managed_matching_server() {
        let mut daemon = DaemonVersion {
            status: "running".into(),
            backend: Some("pid".into()),
            managed_codex_path: None,
            socket_path: None,
            app_server_version: Some("0.167.0".into()),
        };
        assert!(validate_running_identity(&daemon, "codex-cli 0.167.0").is_ok());
        daemon.backend = None;
        assert!(validate_running_identity(&daemon, "codex-cli 0.167.0").is_err());
        daemon.backend = Some("pid".into());
        daemon.app_server_version = Some("0.167.0+rich".into());
        assert!(validate_running_identity(&daemon, "codex-cli 0.167.0").is_err());
    }

    #[test]
    fn delegated_backend_retains_the_consumed_remote_control_restriction() {
        let args = [OsString::from("app-server")];
        let command = official_command(Path::new("/official/codex"), args, true);
        assert!(command.get_envs().any(|(name, value)| {
            name == "CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED"
                && value == Some(std::ffi::OsStr::new("1"))
        }));
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["app-server"]);
    }
}
