//! Version-checked registration of one already-local extension package.
//!
//! Registration is deliberately inert. It re-inspects a previously discovered
//! manifest, verifies the exact local entry-point file and compatibility, and
//! returns canonical installed metadata. It never copies files, starts a
//! process, accepts trust, or enables a contribution.

use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

use aworkit_process::identity::ExecutableIdentityV1;
use semver::{Version, VersionReq};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    extension_inspection::{
        ExtensionInspectionError, ExtensionManifestInspectionV2,
        inspect_extension_manifest_details_v2, reject_symlink_components,
    },
    settings_v2::{ExtensionConfigurationV2, ExtensionStatusV2},
};

const SUPPORTED_PLUGIN_PROTOCOL_VERSION: u16 = 1;
const INSTALLATION_STATE: &str = "registered_inert";
const INTEGRITY_STATE: &str = "verified_entry_point_content";
const CONTENT_HASH_SCOPE: &str = "entry_point_file_v1";

/// Re-inspects and registers one saved discovery as installed metadata.
pub(crate) fn register_extension_installation_v2(
    discovered: &ExtensionConfigurationV2,
) -> Result<ExtensionConfigurationV2, ExtensionRegistrationError> {
    require_discovered_state(discovered)?;
    let inspected = inspect_extension_manifest_details_v2(Path::new(&discovered.manifest_path))?;
    require_same_discovery(discovered, &inspected)?;
    verify_compatibility(&inspected)?;
    let entry_point = inspect_entry_point(&inspected)?;
    if entry_point.content_hash != inspected.manifest.content_hash {
        return Err(ExtensionRegistrationError::ContentHashMismatch);
    }
    let final_inspection =
        inspect_extension_manifest_details_v2(Path::new(&discovered.manifest_path))?;
    require_same_discovery(discovered, &final_inspection)?;
    let final_entry_point = inspect_entry_point(&final_inspection)?;
    if inspected.manifest != final_inspection.manifest || entry_point != final_entry_point {
        return Err(ExtensionRegistrationError::ChangedDuringRegistration);
    }
    let entry_point = final_entry_point;

    let mut configuration = discovered.configuration.clone();
    insert_text(&mut configuration, "installationState", INSTALLATION_STATE);
    insert_text(&mut configuration, "integrityState", INTEGRITY_STATE);
    insert_text(&mut configuration, "contentHashScope", CONTENT_HASH_SCOPE);
    insert_text(
        &mut configuration,
        "entryPointContentHash",
        &entry_point.content_hash,
    );
    insert_text(
        &mut configuration,
        "entryPointIdentity",
        &entry_point.identity_hash,
    );
    insert_text(
        &mut configuration,
        "installedForAworkitVersion",
        env!("CARGO_PKG_VERSION"),
    );

    Ok(ExtensionConfigurationV2 {
        id: discovered.id.clone(),
        name: discovered.name.clone(),
        version: discovered.version.clone(),
        status: ExtensionStatusV2::Installed,
        enabled: false,
        trust_accepted: false,
        manifest_path: inspected.configuration.manifest_path,
        entry_point: Some(entry_point.canonical_path),
        content_hash: Some(entry_point.content_hash),
        compatibility: Some(format!(
            "compatible: Aworkit {} satisfies '{}' and plugin protocol {} is supported",
            env!("CARGO_PKG_VERSION"),
            inspected.manifest.aworkit_version_requirement,
            inspected.manifest.protocol_version
        )),
        provenance: Some(
            "manual local package registered after inert manifest, compatibility, and exact entry-point-file verification; no extension code was executed"
                .into(),
        ),
        configuration,
    })
}

/// Revalidates a registered identity before it may become enabled.
pub(crate) fn verify_registered_extension_v2(
    registered: &ExtensionConfigurationV2,
) -> Result<(), ExtensionRegistrationError> {
    if registered.status != ExtensionStatusV2::Installed {
        return Err(ExtensionRegistrationError::NotInstalled);
    }
    let inspected = inspect_extension_manifest_details_v2(Path::new(&registered.manifest_path))?;
    require_registered_manifest(registered, &inspected)?;
    verify_compatibility(&inspected)?;
    let entry_point = inspect_entry_point(&inspected)?;
    if registered.content_hash.as_deref() != Some(entry_point.content_hash.as_str())
        || entry_point.content_hash != inspected.manifest.content_hash
        || registered.entry_point.as_deref() != Some(entry_point.canonical_path.as_str())
        || registered
            .configuration
            .get("entryPointIdentity")
            .and_then(Value::as_str)
            != Some(entry_point.identity_hash.as_str())
    {
        return Err(ExtensionRegistrationError::InstalledIdentityDrift);
    }
    Ok(())
}

fn require_discovered_state(
    discovered: &ExtensionConfigurationV2,
) -> Result<(), ExtensionRegistrationError> {
    if discovered.status != ExtensionStatusV2::Discovered {
        return Err(ExtensionRegistrationError::NotDiscovered);
    }
    if discovered.enabled || discovered.trust_accepted {
        return Err(ExtensionRegistrationError::DiscoveryNotInert);
    }
    Ok(())
}

fn require_same_discovery(
    discovered: &ExtensionConfigurationV2,
    inspected: &ExtensionManifestInspectionV2,
) -> Result<(), ExtensionRegistrationError> {
    let current = &inspected.configuration;
    let immutable_matches = discovered.id == current.id
        && discovered.version == current.version
        && discovered.manifest_path == current.manifest_path
        && discovered.entry_point == current.entry_point
        && discovered.content_hash == current.content_hash
        && inspection_fact(discovered, "manifestContentHash")
            == inspection_fact(current, "manifestContentHash")
        && inspection_fact(discovered, "pluginProtocolVersion")
            == inspection_fact(current, "pluginProtocolVersion")
        && inspection_fact(discovered, "aworkitVersionRequirement")
            == inspection_fact(current, "aworkitVersionRequirement");
    if immutable_matches {
        Ok(())
    } else {
        Err(ExtensionRegistrationError::DiscoveryDrift)
    }
}

fn require_registered_manifest(
    registered: &ExtensionConfigurationV2,
    inspected: &ExtensionManifestInspectionV2,
) -> Result<(), ExtensionRegistrationError> {
    let current = &inspected.configuration;
    if registered.id != current.id
        || registered.version != current.version
        || registered.manifest_path != current.manifest_path
        || registered.content_hash != current.content_hash
        || inspection_fact(registered, "manifestContentHash")
            != inspection_fact(current, "manifestContentHash")
        || inspection_fact(registered, "pluginProtocolVersion")
            != inspection_fact(current, "pluginProtocolVersion")
        || inspection_fact(registered, "aworkitVersionRequirement")
            != inspection_fact(current, "aworkitVersionRequirement")
    {
        return Err(ExtensionRegistrationError::InstalledIdentityDrift);
    }
    for (key, expected) in [
        ("installationState", INSTALLATION_STATE),
        ("integrityState", INTEGRITY_STATE),
        ("contentHashScope", CONTENT_HASH_SCOPE),
    ] {
        if inspection_fact(registered, key) != Some(expected) {
            return Err(ExtensionRegistrationError::InstalledIdentityDrift);
        }
    }
    Ok(())
}

fn verify_compatibility(
    inspected: &ExtensionManifestInspectionV2,
) -> Result<(), ExtensionRegistrationError> {
    if inspected.manifest.protocol_version != SUPPORTED_PLUGIN_PROTOCOL_VERSION {
        return Err(ExtensionRegistrationError::ProtocolIncompatible {
            found: inspected.manifest.protocol_version,
            supported: SUPPORTED_PLUGIN_PROTOCOL_VERSION,
        });
    }
    let requirement = VersionReq::parse(&inspected.manifest.aworkit_version_requirement)
        .map_err(|_| ExtensionRegistrationError::InvalidAworkitRequirement)?;
    let version = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|_| ExtensionRegistrationError::InvalidApplicationVersion)?;
    if !requirement.matches(&version) {
        return Err(ExtensionRegistrationError::AworkitIncompatible {
            found: env!("CARGO_PKG_VERSION"),
            requirement: inspected.manifest.aworkit_version_requirement.clone(),
        });
    }
    Ok(())
}

#[derive(Eq, PartialEq)]
struct InspectedEntryPoint {
    canonical_path: String,
    content_hash: String,
    identity_hash: String,
}

fn inspect_entry_point(
    inspected: &ExtensionManifestInspectionV2,
) -> Result<InspectedEntryPoint, ExtensionRegistrationError> {
    let declared = Path::new(&inspected.manifest.entry_point.program);
    if declared
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ExtensionRegistrationError::EntryPointTraversal);
    }
    let manifest_path = Path::new(&inspected.configuration.manifest_path);
    let candidate = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        manifest_path
            .parent()
            .ok_or(ExtensionRegistrationError::ManifestHasNoParent)?
            .join(declared)
    };
    reject_symlink_components(&candidate)?;
    let executable = ExecutableIdentityV1::open(&candidate)
        .map_err(|_| ExtensionRegistrationError::EntryPointUnavailable)?;
    require_executable_permission(&executable.canonical_path)?;
    let canonical_path = executable
        .canonical_path
        .to_str()
        .ok_or(ExtensionRegistrationError::EntryPointPathNotUtf8)?
        .to_owned();
    let identity_hash = entry_point_identity_hash(
        &canonical_path,
        &executable.content_hash,
        &inspected.manifest.entry_point.arguments,
    )?;
    Ok(InspectedEntryPoint {
        canonical_path,
        content_hash: executable.content_hash,
        identity_hash,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EntryPointIdentity<'a> {
    canonical_path: &'a str,
    content_hash: &'a str,
    arguments: &'a [String],
}

fn entry_point_identity_hash(
    canonical_path: &str,
    content_hash: &str,
    arguments: &[String],
) -> Result<String, ExtensionRegistrationError> {
    let bytes = serde_jcs::to_vec(&EntryPointIdentity {
        canonical_path,
        content_hash,
        arguments,
    })
    .map_err(|_| ExtensionRegistrationError::EntryPointIdentityEncoding)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

#[cfg(unix)]
fn require_executable_permission(path: &Path) -> Result<(), ExtensionRegistrationError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .map_err(|_| ExtensionRegistrationError::EntryPointUnavailable)?
        .permissions()
        .mode();
    if mode & 0o111 == 0 {
        return Err(ExtensionRegistrationError::EntryPointNotExecutable);
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_executable_permission(_path: &Path) -> Result<(), ExtensionRegistrationError> {
    Ok(())
}

fn inspection_fact<'a>(extension: &'a ExtensionConfigurationV2, key: &str) -> Option<&'a str> {
    extension.configuration.get(key).and_then(Value::as_str)
}

fn insert_text(configuration: &mut BTreeMap<String, Value>, key: &str, value: &str) {
    configuration.insert(key.to_owned(), Value::String(value.to_owned()));
}

/// Failures contain only bounded metadata and never extension output.
#[derive(Debug, Error)]
pub(crate) enum ExtensionRegistrationError {
    #[error("only a disabled, untrusted discovered extension can be registered")]
    NotDiscovered,
    #[error("a discovered extension must remain disabled and untrusted before registration")]
    DiscoveryNotInert,
    #[error("the extension manifest changed after discovery; discover it again before registering")]
    DiscoveryDrift,
    #[error("the extension manifest or entry point changed during registration; try again")]
    ChangedDuringRegistration,
    #[error("the registered extension manifest or entry-point identity changed")]
    InstalledIdentityDrift,
    #[error("the extension is not registered as installed")]
    NotInstalled,
    #[error("plugin protocol {found} is incompatible with supported protocol {supported}")]
    ProtocolIncompatible { found: u16, supported: u16 },
    #[error("the extension declares an invalid Aworkit semantic-version requirement")]
    InvalidAworkitRequirement,
    #[error("this Aworkit build has an invalid semantic version")]
    InvalidApplicationVersion,
    #[error("Aworkit {found} does not satisfy extension requirement '{requirement}'")]
    AworkitIncompatible {
        found: &'static str,
        requirement: String,
    },
    #[error("the extension manifest has no parent directory")]
    ManifestHasNoParent,
    #[error("the extension entry point must not contain parent traversal")]
    EntryPointTraversal,
    #[error("the extension entry point is unavailable or changed during verification")]
    EntryPointUnavailable,
    #[error("the extension entry point path must be valid UTF-8")]
    EntryPointPathNotUtf8,
    #[error("the extension entry point is not executable")]
    // Constructed only by the Unix executable-permission check; Windows has no
    // executable-bit concept, so the variant is never built there.
    #[cfg_attr(not(unix), allow(dead_code))]
    EntryPointNotExecutable,
    #[error("the extension entry-point identity could not be encoded")]
    EntryPointIdentityEncoding,
    #[error("the declared contentHash does not match the exact entry-point file SHA-256")]
    ContentHashMismatch,
    #[error(transparent)]
    Inspection(#[from] ExtensionInspectionError),
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::runtime::extension_inspection::inspect_extension_manifest_v2;

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).expect("make executable");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {}

    fn fixture(requirement: &str) -> (TempDir, PathBuf, PathBuf) {
        let root = TempDir::new().expect("temp package");
        let sentinel = root.path().join("must-not-exist");
        let entry_point = root.path().join("extension-entry");
        fs::write(
            &entry_point,
            format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
        )
        .expect("write entry point");
        make_executable(&entry_point);
        let content_hash = format!(
            "sha256:{:x}",
            Sha256::digest(fs::read(&entry_point).expect("read entry point"))
        );
        let manifest_path = root.path().join("extension.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "extensionId": "extension.fixture",
                "version": "1.0.0",
                "contentHash": content_hash,
                "aworkitVersionRequirement": requirement,
                "protocolVersion": 1,
                "entryPoint": {"program": "extension-entry", "arguments": ["--stdio"]},
                "contributions": [{
                    "contributionId": "tool.fixture",
                    "kind": "tool",
                    "inputSchema": {"type": "object"},
                    "outputSchema": {"type": "object"}
                }],
                "dependencies": []
            }))
            .expect("manifest JSON"),
        )
        .expect("write manifest");
        (root, manifest_path, sentinel)
    }

    #[test]
    fn registration_verifies_but_never_executes_and_remains_disabled_untrusted() {
        let (_root, manifest_path, sentinel) = fixture(">=0.1.0,<0.2.0");
        let discovered = inspect_extension_manifest_v2(&manifest_path).expect("discover");

        let installed =
            register_extension_installation_v2(&discovered).expect("register installation");

        assert_eq!(installed.status, ExtensionStatusV2::Installed);
        assert!(!installed.enabled);
        assert!(!installed.trust_accepted);
        assert!(!sentinel.exists(), "registration must never execute code");
        assert_eq!(
            inspection_fact(&installed, "installationState"),
            Some(INSTALLATION_STATE)
        );
        assert_eq!(
            inspection_fact(&installed, "integrityState"),
            Some(INTEGRITY_STATE)
        );
        verify_registered_extension_v2(&installed).expect("revalidate exact identity");
    }

    #[test]
    fn registration_rejects_manifest_entry_point_and_compatibility_drift() {
        let (_root, manifest_path, _sentinel) = fixture(">=0.1.0,<0.2.0");
        let discovered = inspect_extension_manifest_v2(&manifest_path).expect("discover");
        fs::write(&manifest_path, b"{}").expect("replace manifest");
        assert!(matches!(
            register_extension_installation_v2(&discovered),
            Err(ExtensionRegistrationError::Inspection(_))
                | Err(ExtensionRegistrationError::DiscoveryDrift)
        ));

        let (_root, manifest_path, _sentinel) = fixture(">=9.0.0");
        let discovered = inspect_extension_manifest_v2(&manifest_path).expect("discover");
        assert!(matches!(
            register_extension_installation_v2(&discovered),
            Err(ExtensionRegistrationError::AworkitIncompatible { .. })
        ));
    }

    #[test]
    fn revalidation_rejects_changed_entry_point_content() {
        let (_root, manifest_path, _sentinel) = fixture(">=0.1.0,<0.2.0");
        let discovered = inspect_extension_manifest_v2(&manifest_path).expect("discover");
        let installed =
            register_extension_installation_v2(&discovered).expect("register installation");
        let entry_point = Path::new(installed.entry_point.as_deref().expect("entry point"));
        fs::write(entry_point, b"#!/bin/sh\nexit 0\n").expect("mutate entry point");
        make_executable(entry_point);

        assert!(matches!(
            verify_registered_extension_v2(&installed),
            Err(ExtensionRegistrationError::InstalledIdentityDrift)
        ));
    }
}
