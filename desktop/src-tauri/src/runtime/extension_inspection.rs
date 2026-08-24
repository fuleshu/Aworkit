//! Bounded, inert inspection of one user-selected extension manifest.
//!
//! Inspection reads JSON metadata only. It does not resolve the declared entry
//! point, read credentials, install a package, accept trust, or start code.

use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use aworkit_capability_host::{
    ExtensionManifestV1, PluginManifestError, PluginManifestLimitsV1, parse_extension_manifest_v1,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::settings_v2::{ExtensionConfigurationV2, ExtensionStatusV2};

const MAXIMUM_MANIFEST_PATH_BYTES: usize = 4 * 1_024;
const MAXIMUM_MANIFEST_PATH_COMPONENTS: usize = 256;
const SUPPORTED_PLUGIN_PROTOCOL_VERSION: u16 = 1;

/// Reads and projects one local extension manifest without activating it.
///
/// The returned Settings v2 record is always disabled and untrusted. A
/// protocol mismatch is reported as incompatible; otherwise the record stays
/// discovered because full Aworkit-version and package-integrity checks belong
/// to the separate installation operation.
pub(crate) fn inspect_extension_manifest_v2(
    manifest_path: &Path,
) -> Result<ExtensionConfigurationV2, ExtensionInspectionError> {
    Ok(inspect_extension_manifest_details_v2(manifest_path)?.configuration)
}

/// Complete inert facts retained only while a dedicated registration command
/// verifies that a prior discovery still identifies the same local package.
pub(crate) struct ExtensionManifestInspectionV2 {
    pub(crate) configuration: ExtensionConfigurationV2,
    pub(crate) manifest: ExtensionManifestV1,
}

/// Re-reads a manifest with the same bounded, no-follow checks as discovery.
/// Returning the parsed manifest does not resolve or execute its entry point.
pub(crate) fn inspect_extension_manifest_details_v2(
    manifest_path: &Path,
) -> Result<ExtensionManifestInspectionV2, ExtensionInspectionError> {
    validate_selected_path(manifest_path)?;
    reject_symlink_components(manifest_path)?;

    let canonical_path =
        fs::canonicalize(manifest_path).map_err(|source| ExtensionInspectionError::FileSystem {
            operation: "canonicalize the extension manifest",
            source,
        })?;
    let canonical_text = validated_path_text(&canonical_path)?;

    let path_metadata = fs::symlink_metadata(&canonical_path).map_err(|source| {
        ExtensionInspectionError::FileSystem {
            operation: "inspect extension manifest metadata",
            source,
        }
    })?;
    if path_metadata.file_type().is_symlink() {
        return Err(ExtensionInspectionError::SymbolicLink);
    }
    if !path_metadata.is_file() {
        return Err(ExtensionInspectionError::NotRegularFile);
    }

    let limits = PluginManifestLimitsV1::default();
    validate_declared_size(&path_metadata, limits.maximum_manifest_bytes)?;

    let mut file =
        File::open(&canonical_path).map_err(|source| ExtensionInspectionError::FileSystem {
            operation: "open the extension manifest",
            source,
        })?;
    let opened_metadata =
        file.metadata()
            .map_err(|source| ExtensionInspectionError::FileSystem {
                operation: "inspect the opened extension manifest",
                source,
            })?;
    if !opened_metadata.is_file() {
        return Err(ExtensionInspectionError::NotRegularFile);
    }
    if !same_file_identity(&path_metadata, &opened_metadata) {
        return Err(ExtensionInspectionError::ChangedDuringInspection);
    }
    validate_declared_size(&opened_metadata, limits.maximum_manifest_bytes)?;

    let read_bound = limits.maximum_manifest_bytes.saturating_add(1);
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened_metadata.len())
            .unwrap_or(limits.maximum_manifest_bytes)
            .min(limits.maximum_manifest_bytes),
    );
    (&mut file)
        .take(u64::try_from(read_bound).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|source| ExtensionInspectionError::FileSystem {
            operation: "read the extension manifest",
            source,
        })?;
    if bytes.is_empty() {
        return Err(ExtensionInspectionError::EmptyManifest);
    }
    if bytes.len() > limits.maximum_manifest_bytes {
        return Err(ExtensionInspectionError::ManifestTooLarge {
            maximum_bytes: limits.maximum_manifest_bytes,
        });
    }

    let final_open_metadata =
        file.metadata()
            .map_err(|source| ExtensionInspectionError::FileSystem {
                operation: "reinspect the opened extension manifest",
                source,
            })?;
    let final_path_metadata = fs::symlink_metadata(&canonical_path).map_err(|source| {
        ExtensionInspectionError::FileSystem {
            operation: "reinspect the extension manifest path",
            source,
        }
    })?;
    if final_path_metadata.file_type().is_symlink()
        || !final_path_metadata.is_file()
        || !same_file_identity(&opened_metadata, &final_open_metadata)
        || !same_file_identity(&opened_metadata, &final_path_metadata)
        || opened_metadata.len() != final_open_metadata.len()
        || opened_metadata.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
        || metadata_changed(&opened_metadata, &final_open_metadata)
    {
        return Err(ExtensionInspectionError::ChangedDuringInspection);
    }

    let observed_manifest_hash = format!("sha256:{:x}", Sha256::digest(&bytes));
    let manifest = parse_extension_manifest_v1(&bytes, limits)?;
    let configuration = project_discovery(manifest.clone(), canonical_text, observed_manifest_hash);
    Ok(ExtensionManifestInspectionV2 {
        configuration,
        manifest,
    })
}

fn validate_selected_path(path: &Path) -> Result<(), ExtensionInspectionError> {
    let text = validated_path_text(path)?;
    if !path.is_absolute() {
        return Err(ExtensionInspectionError::PathNotAbsolute);
    }
    if text.contains('\0') {
        return Err(ExtensionInspectionError::InvalidPathText);
    }
    let component_count = path.components().count();
    if component_count > MAXIMUM_MANIFEST_PATH_COMPONENTS {
        return Err(ExtensionInspectionError::TooManyPathComponents {
            maximum: MAXIMUM_MANIFEST_PATH_COMPONENTS,
        });
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ExtensionInspectionError::ParentTraversal);
    }
    Ok(())
}

fn validated_path_text(path: &Path) -> Result<String, ExtensionInspectionError> {
    let text = path.to_str().ok_or(ExtensionInspectionError::PathNotUtf8)?;
    if text.is_empty() || text.len() > MAXIMUM_MANIFEST_PATH_BYTES {
        return Err(ExtensionInspectionError::InvalidPathLength {
            maximum_bytes: MAXIMUM_MANIFEST_PATH_BYTES,
        });
    }
    Ok(text.to_owned())
}

pub(crate) fn reject_symlink_components(path: &Path) -> Result<(), ExtensionInspectionError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => return Err(ExtensionInspectionError::ParentTraversal),
            Component::Normal(part) => {
                current.push(part);
                let metadata = fs::symlink_metadata(&current).map_err(|source| {
                    ExtensionInspectionError::FileSystem {
                        operation: "inspect an extension manifest path component",
                        source,
                    }
                })?;
                if metadata.file_type().is_symlink() {
                    return Err(ExtensionInspectionError::SymbolicLink);
                }
            }
        }
    }
    Ok(())
}

fn validate_declared_size(
    metadata: &Metadata,
    maximum_bytes: usize,
) -> Result<(), ExtensionInspectionError> {
    if metadata.len() == 0 {
        return Err(ExtensionInspectionError::EmptyManifest);
    }
    if metadata.len() > u64::try_from(maximum_bytes).unwrap_or(u64::MAX) {
        return Err(ExtensionInspectionError::ManifestTooLarge { maximum_bytes });
    }
    Ok(())
}

#[cfg(unix)]
fn same_file_identity(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(_left: &Metadata, _right: &Metadata) -> bool {
    // The before/after type, length, and modification checks still fail closed
    // for ordinary replacement races on platforms without a portable file ID.
    true
}

fn metadata_changed(before: &Metadata, after: &Metadata) -> bool {
    match (before.modified(), after.modified()) {
        (Ok(before), Ok(after)) => before != after,
        _ => false,
    }
}

fn project_discovery(
    manifest: ExtensionManifestV1,
    manifest_path: String,
    observed_manifest_hash: String,
) -> ExtensionConfigurationV2 {
    let protocol_supported = manifest.protocol_version == SUPPORTED_PLUGIN_PROTOCOL_VERSION;
    let status = if protocol_supported {
        ExtensionStatusV2::Discovered
    } else {
        ExtensionStatusV2::Incompatible
    };
    let compatibility = if protocol_supported {
        format!(
            "discovered: plugin protocol {} matches this build; declared Aworkit requirement '{}' remains unchecked until installation",
            manifest.protocol_version, manifest.aworkit_version_requirement
        )
    } else {
        format!(
            "incompatible: plugin protocol {} is not supported by this build (supported: {}); declared Aworkit requirement '{}'",
            manifest.protocol_version,
            SUPPORTED_PLUGIN_PROTOCOL_VERSION,
            manifest.aworkit_version_requirement
        )
    };
    let provenance = format!(
        "inert local-manifest inspection; observed manifest hash {observed_manifest_hash}; declared package content hash was not verified; no extension code was executed"
    );
    let configuration = BTreeMap::from([
        (
            "aworkitVersionRequirement".to_owned(),
            Value::String(manifest.aworkit_version_requirement.clone()),
        ),
        (
            "contributionCount".to_owned(),
            json!(manifest.contributions.len()),
        ),
        (
            "dependencyCount".to_owned(),
            json!(manifest.dependencies.len()),
        ),
        (
            "inspectionMode".to_owned(),
            Value::String("inert_manifest_only".to_owned()),
        ),
        (
            "integrityState".to_owned(),
            Value::String("declared_content_hash_unverified".to_owned()),
        ),
        (
            "manifestContentHash".to_owned(),
            Value::String(observed_manifest_hash),
        ),
        (
            "pluginProtocolVersion".to_owned(),
            json!(manifest.protocol_version),
        ),
    ]);

    ExtensionConfigurationV2 {
        id: manifest.extension_id.as_str().to_owned(),
        name: manifest.extension_id.as_str().to_owned(),
        version: manifest.version,
        status,
        enabled: false,
        trust_accepted: false,
        manifest_path,
        entry_point: Some(manifest.entry_point.program),
        content_hash: Some(manifest.content_hash),
        compatibility: Some(compatibility),
        provenance: Some(provenance),
        configuration,
    }
}

/// Failures are intentionally metadata-only: no manifest content or credential
/// value is included in an error.
#[derive(Debug, Error)]
pub(crate) enum ExtensionInspectionError {
    #[error("extension manifest path must be absolute")]
    PathNotAbsolute,
    #[error("extension manifest path must be valid UTF-8")]
    PathNotUtf8,
    #[error("extension manifest path contains invalid text")]
    InvalidPathText,
    #[error("extension manifest path is empty or exceeds the {maximum_bytes}-byte limit")]
    InvalidPathLength { maximum_bytes: usize },
    #[error("extension manifest path exceeds the {maximum}-component limit")]
    TooManyPathComponents { maximum: usize },
    #[error("extension manifest path must not contain parent traversal")]
    ParentTraversal,
    #[error("extension manifest path must not contain symbolic links")]
    SymbolicLink,
    #[error("extension manifest path must identify a regular file")]
    NotRegularFile,
    #[error("extension manifest is empty")]
    EmptyManifest,
    #[error("extension manifest exceeds the {maximum_bytes}-byte limit")]
    ManifestTooLarge { maximum_bytes: usize },
    #[error("extension manifest changed while it was being inspected")]
    ChangedDuringInspection,
    #[error("could not {operation}: {source}")]
    FileSystem {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("extension manifest is invalid: {0}")]
    Manifest(#[from] PluginManifestError),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::runtime::settings_v2::SettingsConfigurationV2;

    fn content_hash(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn manifest(protocol_version: u16, program: &str, arguments: &[&str]) -> Value {
        json!({
            "schemaVersion": 1,
            "extensionId": "extension.review",
            "version": "2.3.1",
            "contentHash": content_hash('a'),
            "aworkitVersionRequirement": ">=0.1.0,<0.2.0",
            "protocolVersion": protocol_version,
            "entryPoint": {
                "program": program,
                "arguments": arguments
            },
            "contributions": [{
                "contributionId": "tool.review",
                "kind": "tool",
                "inputSchema": {"type": "object"},
                "outputSchema": {"type": "object"}
            }],
            "dependencies": []
        })
    }

    fn write_manifest(
        directory: &TempDir,
        protocol_version: u16,
        program: &str,
        arguments: &[&str],
    ) -> PathBuf {
        let path = directory.path().join("extension.json");
        fs::write(
            &path,
            serde_json::to_vec(&manifest(protocol_version, program, arguments))
                .expect("encode fixture"),
        )
        .expect("write fixture");
        path
    }

    #[test]
    fn valid_manifest_projects_disabled_untrusted_discovery_without_execution() {
        let directory = TempDir::new().expect("temporary directory");
        let sentinel = directory.path().join("must-not-exist");
        let script = directory.path().join("entry-point.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
        )
        .expect("write inert entry point");
        let manifest_path = write_manifest(
            &directory,
            SUPPORTED_PLUGIN_PROTOCOL_VERSION,
            script.to_str().expect("UTF-8 fixture path"),
            &["--plugin-protocol", "do-not-project-this-argument"],
        );

        let discovered =
            inspect_extension_manifest_v2(&manifest_path).expect("inspect valid manifest");

        assert_eq!(discovered.id, "extension.review");
        assert_eq!(discovered.name, "extension.review");
        assert_eq!(discovered.version, "2.3.1");
        assert_eq!(discovered.status, ExtensionStatusV2::Discovered);
        assert!(!discovered.enabled);
        assert!(!discovered.trust_accepted);
        assert_eq!(discovered.entry_point.as_deref(), script.to_str());
        assert_eq!(
            discovered.content_hash.as_deref(),
            Some(content_hash('a').as_str())
        );
        assert!(
            discovered
                .compatibility
                .as_deref()
                .is_some_and(|value| value.contains("remains unchecked until installation"))
        );
        assert!(
            discovered
                .provenance
                .as_deref()
                .is_some_and(|value| value.contains("no extension code was executed"))
        );
        assert!(
            !sentinel.exists(),
            "inspection must never execute the entry point"
        );

        let encoded = serde_json::to_string(&discovered).expect("serialize discovery");
        assert!(!encoded.contains("do-not-project-this-argument"));
        assert!(!encoded.contains("inputSchema"));
        assert!(!encoded.contains("outputSchema"));

        let mut settings = SettingsConfigurationV2::default();
        settings.extensions.push(discovered);
        settings.validate().expect("valid Settings v2 projection");
    }

    #[test]
    fn unsupported_plugin_protocol_is_retained_as_incompatible() {
        let directory = TempDir::new().expect("temporary directory");
        let path = write_manifest(&directory, 2, "/not/resolved/or/executed", &[]);

        let discovered = inspect_extension_manifest_v2(&path).expect("inspect metadata");

        assert_eq!(discovered.status, ExtensionStatusV2::Incompatible);
        assert!(!discovered.enabled);
        assert!(!discovered.trust_accepted);
        assert!(
            discovered
                .compatibility
                .as_deref()
                .is_some_and(|value| value.contains("plugin protocol 2 is not supported"))
        );
    }

    #[test]
    fn rejects_relative_parent_traversal_and_non_files() {
        assert!(matches!(
            inspect_extension_manifest_v2(Path::new("extension.json")),
            Err(ExtensionInspectionError::PathNotAbsolute)
        ));
        assert!(matches!(
            inspect_extension_manifest_v2(Path::new("/tmp/../extension.json")),
            Err(ExtensionInspectionError::ParentTraversal)
        ));

        let directory = TempDir::new().expect("temporary directory");
        assert!(matches!(
            inspect_extension_manifest_v2(directory.path()),
            Err(ExtensionInspectionError::NotRegularFile)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_file_and_directory_components() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().expect("temporary directory");
        let path = write_manifest(&directory, 1, "/not/executed", &[]);
        let file_link = directory.path().join("manifest-link.json");
        symlink(&path, &file_link).expect("create file symlink");
        assert!(matches!(
            inspect_extension_manifest_v2(&file_link),
            Err(ExtensionInspectionError::SymbolicLink)
        ));

        let parent = TempDir::new().expect("temporary symlink parent");
        let directory_link = parent.path().join("extension-dir");
        symlink(directory.path(), &directory_link).expect("create directory symlink");
        assert!(matches!(
            inspect_extension_manifest_v2(&directory_link.join("extension.json")),
            Err(ExtensionInspectionError::SymbolicLink)
        ));
    }

    #[test]
    fn rejects_empty_oversized_and_malformed_manifests() {
        let directory = TempDir::new().expect("temporary directory");
        let empty = directory.path().join("empty.json");
        fs::write(&empty, []).expect("write empty fixture");
        assert!(matches!(
            inspect_extension_manifest_v2(&empty),
            Err(ExtensionInspectionError::EmptyManifest)
        ));

        let oversized = directory.path().join("oversized.json");
        fs::write(
            &oversized,
            vec![b' '; PluginManifestLimitsV1::default().maximum_manifest_bytes + 1],
        )
        .expect("write oversized fixture");
        assert!(matches!(
            inspect_extension_manifest_v2(&oversized),
            Err(ExtensionInspectionError::ManifestTooLarge { .. })
        ));

        let malformed = directory.path().join("malformed.json");
        fs::write(&malformed, br#"{"schemaVersion":1}"#).expect("write malformed fixture");
        assert!(matches!(
            inspect_extension_manifest_v2(&malformed),
            Err(ExtensionInspectionError::Manifest(_))
        ));
    }
}
