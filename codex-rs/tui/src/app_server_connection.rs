//! Applies the selected server's socket policy on initial connections and reconnects.

use crate::AppServerTarget;
#[cfg(windows)]
use crate::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
use crate::connect_remote_app_server;
use codex_app_server_client::AppServerClient;
#[cfg(windows)]
use codex_app_server_client::RemoteAppServerClient;
#[cfg(windows)]
use codex_app_server_client::RemoteAppServerConnectArgs;
#[cfg(windows)]
use codex_app_server_client::RemoteAppServerEndpoint;
#[cfg(windows)]
use codex_utils_absolute_path::AbsolutePathBuf;

pub(crate) fn require_external_backend(
    required: bool,
    target: &AppServerTarget,
) -> std::io::Result<()> {
    if required
        && matches!(
            target,
            AppServerTarget::Embedded
                | AppServerTarget::LocalDaemon {
                    allow_embedded_fallback: true,
                    ..
                }
        )
    {
        return Err(std::io::Error::other(
            "Codex Rich cannot fall back to an embedded credential backend. Reconnect to the official app server.",
        ));
    }
    Ok(())
}

pub(crate) fn require_supported_official_launch(
    required: bool,
    exclusion: Option<&str>,
) -> std::io::Result<()> {
    if required && let Some(reason) = exclusion {
        return Err(std::io::Error::other(format!(
            "Codex Rich cannot apply {reason} through the shared official backend. Configure these settings on the official server instead.",
        )));
    }
    Ok(())
}

pub(crate) async fn connect(target: &AppServerTarget) -> color_eyre::Result<AppServerClient> {
    match target {
        AppServerTarget::Embedded => {
            color_eyre::eyre::bail!("embedded sessions have no remote connection")
        }
        #[cfg(target_os = "macos")]
        AppServerTarget::LocalDaemon { endpoint, .. }
            if option_env!("CODEX_RICH_VERSION").is_some() =>
        {
            crate::connect_official_local_app_server(endpoint.clone()).await
        }
        #[cfg(windows)]
        AppServerTarget::LocalDaemon {
            endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
            ..
        } => {
            // Revalidate at the real connection, not just the earlier discovery probe.
            let (socket_path, _directory) =
                codex_uds::validate_private_socket_path(socket_path.as_path())?;
            let app_server =
                RemoteAppServerClient::connect_local_daemon(RemoteAppServerConnectArgs {
                    required_human_approval: true,
                    endpoint: RemoteAppServerEndpoint::UnixSocket {
                        socket_path: AbsolutePathBuf::from_absolute_path_checked(socket_path)?,
                    },
                    client_name: "codex-tui".to_string(),
                    client_version: env!("CARGO_PKG_VERSION").to_string(),
                    experimental_api: true,
                    mcp_server_openai_form_elicitation: false,
                    opt_out_notification_methods: Vec::new(),
                    channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
                })
                .await?;
            Ok(AppServerClient::Remote(app_server))
        }
        AppServerTarget::LocalDaemon { endpoint, .. } | AppServerTarget::Remote { endpoint } => {
            connect_remote_app_server(endpoint.clone()).await
        }
    }
}

#[cfg(test)]
mod official_backend_tests {
    use super::*;

    fn local_target(fallback: bool) -> AppServerTarget {
        AppServerTarget::LocalDaemon {
            endpoint: codex_app_server_client::RemoteAppServerEndpoint::UnixSocket {
                socket_path: codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
                    std::env::temp_dir().join("codex-rich-policy-test.sock"),
                )
                .expect("absolute temporary path"),
            },
            allow_embedded_fallback: fallback,
        }
    }

    #[test]
    fn official_policy_rejects_embedded_and_fallback_targets() {
        assert!(require_external_backend(true, &AppServerTarget::Embedded).is_err());
        assert!(require_external_backend(true, &local_target(true)).is_err());
        assert!(require_external_backend(true, &local_target(false)).is_ok());
    }

    #[test]
    fn ordinary_launch_policy_keeps_embedded_support() {
        assert!(require_external_backend(false, &AppServerTarget::Embedded).is_ok());
        assert!(require_external_backend(false, &local_target(true)).is_ok());
    }

    #[test]
    fn official_policy_rejects_settings_that_require_an_embedded_launch() {
        assert!(require_supported_official_launch(true, Some("--profile")).is_err());
        assert!(require_supported_official_launch(true, None).is_ok());
        assert!(require_supported_official_launch(false, Some("--profile")).is_ok());
    }
}
