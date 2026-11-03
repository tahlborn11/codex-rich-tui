//! macOS validation for the process at the other end of a local daemon socket.

use std::io::Error;
use std::io::ErrorKind;
use std::io::Result as IoResult;

use codex_uds::UnixStream;
use core_foundation::base::TCFType;
use core_foundation::data::CFData;
use security_framework::os::macos::code_signing::Flags;
use security_framework::os::macos::code_signing::GuestAttributes;
use security_framework::os::macos::code_signing::SecCode;
use security_framework::os::macos::code_signing::SecRequirement;

const OPENAI_CODE_REQUIREMENT: &str = "identifier \"codex\" and anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"";

pub(super) fn verify_server_version(version: Option<&str>) -> IoResult<()> {
    let family = |version: &str| -> Option<(u64, u64)> {
        let mut parts = version.split(['-', '+']).next()?.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let _: u64 = parts.next()?.parse().ok()?;
        (parts.next().is_none()).then_some((major, minor))
    };
    if version.and_then(family) != family(env!("CARGO_PKG_VERSION")) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "official backend version {:?} is incompatible with Rich's upstream version {}; update the frontend or official daemon before reconnecting",
                version,
                env!("CARGO_PKG_VERSION"),
            ),
        ));
    }
    Ok(())
}

/// Validates the actual process bound to `stream`, not the executable selected
/// to launch it. This intentionally performs no Keychain or authentication
/// operation.
pub(super) fn verify_openai_signed_peer(stream: &UnixStream) -> IoResult<()> {
    let audit_token = stream.peer_audit_token().map_err(|err| {
        Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "official OpenAI-signed backend is required: unable to inspect socket peer: {err}"
            ),
        )
    })?;
    let audit_token_data = CFData::from_buffer(&audit_token);
    let mut attributes = GuestAttributes::new();
    attributes.set_audit_token(audit_token_data.as_concrete_TypeRef());

    let code = SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE).map_err(|err| {
        Error::new(
            ErrorKind::PermissionDenied,
            format!("official OpenAI-signed backend is required: unable to identify socket peer: {err}"),
        )
    })?;
    let requirement = OPENAI_CODE_REQUIREMENT
        .parse::<SecRequirement>()
        .map_err(|err| {
            Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "official OpenAI-signed backend is required: invalid signing requirement: {err}"
                ),
            )
        })?;
    code.check_validity(
        Flags::STRICT_VALIDATE | Flags::NO_NETWORK_ACCESS,
        &requirement,
    )
    .map_err(|err| {
        Error::new(
            ErrorKind::PermissionDenied,
            format!("official OpenAI-signed backend is required: socket peer failed signing validation: {err}"),
        )
    })
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn official_connection_requires_a_reported_matching_version_family() {
        assert!(verify_server_version(Some("0.166.0")).is_ok());
        assert!(verify_server_version(Some("0.166.0-alpha.5.1")).is_ok());
        assert!(verify_server_version(None).is_err());
        assert!(verify_server_version(Some("0.167.0")).is_err());
        assert!(verify_server_version(Some("0.164.0")).is_err());
        assert!(verify_server_version(Some("0.166.invalid")).is_err());
    }
}
