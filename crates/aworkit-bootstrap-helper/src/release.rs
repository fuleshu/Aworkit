//! Reproducible whole-application release bundle assembly.

use std::{
    fs,
    path::{Path, PathBuf},
};

use aworkit_protocol::{
    ActivationEligibilityV1, BuildOriginV1, BuildProvenanceV1, EXTENSION_HOST_PROTOCOL_V1,
    EXTENSION_MANIFEST_SCHEMA_V1, REPAIR_SCHEMA_VERSION_V1,
};
use filetime::FileTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct WholeBundleInputsV1 {
    pub output: PathBuf,
    pub desktop: PathBuf,
    pub trusted_core: PathBuf,
    pub workflow_worker: PathBuf,
    pub capability_host: PathBuf,
    pub bootstrap_helper: PathBuf,
    pub ui_dist: PathBuf,
    pub source_revision: String,
    pub source_tree_hash: String,
    pub workspace_identity_hash: String,
    pub toolchain_hash: String,
    pub source_date_epoch: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleEntryV1 {
    pub role: String,
    pub relative_path: String,
    pub content_hash: String,
    pub byte_size: u64,
    pub executable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolCompatibilityV1 {
    pub desktop_command_schema: u16,
    pub repair_bootstrap_schema: u16,
    pub extension_manifest_schema: u16,
    pub extension_host_protocol: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WholeApplicationBundleV1 {
    pub schema_version: u16,
    pub platform: String,
    pub architecture: String,
    pub source_date_epoch: i64,
    pub protocol_compatibility: ProtocolCompatibilityV1,
    pub build_origin: BuildOriginV1,
    pub managed_local_self_activation: ActivationEligibilityV1,
    pub self_activation_reason: String,
    pub entries: Vec<BundleEntryV1>,
    pub provenance: BuildProvenanceV1,
}

pub fn assemble_whole_application_bundle(
    inputs: &WholeBundleInputsV1,
) -> Result<WholeApplicationBundleV1, ReleaseBundleError> {
    validate_inputs(inputs)?;
    let parent = inputs
        .output
        .parent()
        .ok_or(ReleaseBundleError::InvalidOutput)?;
    fs::create_dir_all(parent)?;
    if inputs.output.exists() {
        return Err(ReleaseBundleError::OutputExists);
    }
    let staging = unique_staging_path(&inputs.output)?;
    fs::create_dir(&staging)?;
    fs::create_dir(staging.join("bin"))?;
    fs::create_dir(staging.join("ui"))?;

    let result = assemble_in_staging(inputs, &staging);
    let manifest = match result {
        Ok(manifest) => manifest,
        Err(error) => {
            // Staging is uniquely created by this operation and contains no
            // user-owned files. Best-effort cleanup cannot affect the target.
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    normalize_tree_time(&staging, inputs.source_date_epoch)?;
    fs::rename(&staging, &inputs.output)?;
    Ok(manifest)
}

fn assemble_in_staging(
    inputs: &WholeBundleInputsV1,
    staging: &Path,
) -> Result<WholeApplicationBundleV1, ReleaseBundleError> {
    let executable_inputs = [
        ("desktop", &inputs.desktop),
        ("trusted_core", &inputs.trusted_core),
        ("workflow_worker", &inputs.workflow_worker),
        ("capability_host", &inputs.capability_host),
        ("bootstrap_helper", &inputs.bootstrap_helper),
    ];
    let mut entries = Vec::new();
    for (role, source) in executable_inputs {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(ReleaseBundleError::InvalidInput("executable name"))?;
        let relative = PathBuf::from("bin").join(name);
        let destination = staging.join(&relative);
        fs::copy(source, &destination)?;
        set_executable_permissions(&destination)?;
        entries.push(entry(role, &relative, &destination, true)?);
    }

    let mut ui_files = Vec::new();
    collect_files(&inputs.ui_dist, &inputs.ui_dist, &mut ui_files)?;
    ui_files.sort();
    if ui_files.is_empty() {
        return Err(ReleaseBundleError::InvalidInput("empty UI distribution"));
    }
    for relative_ui in ui_files {
        let source = inputs.ui_dist.join(&relative_ui);
        let relative = PathBuf::from("ui").join(&relative_ui);
        let destination = staging.join(&relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, &destination)?;
        set_data_permissions(&destination)?;
        entries.push(entry("desktop_ui", &relative, &destination, false)?);
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let compatibility = ProtocolCompatibilityV1 {
        desktop_command_schema: 1,
        repair_bootstrap_schema: REPAIR_SCHEMA_VERSION_V1,
        extension_manifest_schema: EXTENSION_MANIFEST_SCHEMA_V1,
        extension_host_protocol: EXTENSION_HOST_PROTOCOL_V1,
    };
    let build_origin = BuildOriginV1::PackagedDistribution {
        owner: "aworkit-release-bundle-v1".to_owned(),
    };
    let build_manifest_hash = canonical_hash(&(
        1_u16,
        std::env::consts::OS,
        std::env::consts::ARCH,
        inputs.source_date_epoch,
        &compatibility,
        &build_origin,
        ActivationEligibilityV1::PackagedDistribution,
        &entries,
    ))?;
    let mut provenance = BuildProvenanceV1 {
        source_revision: inputs.source_revision.clone(),
        source_tree_hash: normalize_hash(&inputs.source_tree_hash)?,
        workspace_identity_hash: normalize_hash(&inputs.workspace_identity_hash)?,
        toolchain_hash: normalize_hash(&inputs.toolchain_hash)?,
        build_manifest_hash,
        provenance_hash: String::new(),
    };
    provenance.provenance_hash = canonical_hash(&(
        &provenance.source_revision,
        &provenance.source_tree_hash,
        &provenance.workspace_identity_hash,
        &provenance.toolchain_hash,
        &provenance.build_manifest_hash,
    ))?;
    let manifest = WholeApplicationBundleV1 {
        schema_version: 1,
        platform: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        source_date_epoch: inputs.source_date_epoch,
        protocol_compatibility: compatibility,
        build_origin,
        managed_local_self_activation: ActivationEligibilityV1::PackagedDistribution,
        self_activation_reason: "Packaged layouts are package-owned and cannot use managed-local self-activation unless separately and explicitly enrolled.".to_owned(),
        entries,
        provenance: provenance.clone(),
    };
    write_json(staging.join("BuildProvenanceV1.json"), &provenance)?;
    write_json(staging.join("WholeApplicationBundleV1.json"), &manifest)?;
    Ok(manifest)
}

fn validate_inputs(inputs: &WholeBundleInputsV1) -> Result<(), ReleaseBundleError> {
    for path in [
        &inputs.desktop,
        &inputs.trusted_core,
        &inputs.workflow_worker,
        &inputs.capability_host,
        &inputs.bootstrap_helper,
    ] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(ReleaseBundleError::InvalidInput("binary must be a file"));
        }
    }
    let ui = fs::symlink_metadata(&inputs.ui_dist)?;
    if !ui.file_type().is_dir() || ui.file_type().is_symlink() {
        return Err(ReleaseBundleError::InvalidInput(
            "UI distribution must be a directory",
        ));
    }
    if inputs.source_revision.trim().is_empty() || inputs.source_revision.len() > 256 {
        return Err(ReleaseBundleError::InvalidInput("source revision"));
    }
    if inputs.source_date_epoch < 0 {
        return Err(ReleaseBundleError::InvalidInput("source date epoch"));
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    current: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), ReleaseBundleError> {
    let mut children = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let metadata = child.file_type()?;
        if metadata.is_symlink() {
            return Err(ReleaseBundleError::InvalidInput("UI symlink"));
        }
        if metadata.is_dir() {
            collect_files(root, &child.path(), output)?;
        } else if metadata.is_file() {
            output.push(
                child
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| ReleaseBundleError::InvalidInput("UI path"))?
                    .to_path_buf(),
            );
        } else {
            return Err(ReleaseBundleError::InvalidInput("UI special file"));
        }
    }
    Ok(())
}

fn entry(
    role: &str,
    relative: &Path,
    path: &Path,
    executable: bool,
) -> Result<BundleEntryV1, ReleaseBundleError> {
    let bytes = fs::read(path)?;
    Ok(BundleEntryV1 {
        role: role.to_owned(),
        relative_path: relative
            .to_str()
            .ok_or(ReleaseBundleError::InvalidInput("non-UTF-8 path"))?
            .replace('\\', "/"),
        content_hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
        byte_size: bytes.len() as u64,
        executable,
    })
}

fn write_json(path: PathBuf, value: &impl Serialize) -> Result<(), ReleaseBundleError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(&path, bytes)?;
    set_data_permissions(&path)?;
    Ok(())
}

fn normalize_hash(value: &str) -> Result<String, ReleaseBundleError> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ReleaseBundleError::InvalidInput("SHA-256 digest"));
    }
    Ok(format!("sha256:{}", value.to_ascii_lowercase()))
}

fn canonical_hash(value: &impl Serialize) -> Result<String, ReleaseBundleError> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_jcs::to_vec(value)?)
    ))
}

fn unique_staging_path(output: &Path) -> Result<PathBuf, ReleaseBundleError> {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(|_| ReleaseBundleError::RandomUnavailable)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(ReleaseBundleError::InvalidOutput)?;
    Ok(output.with_file_name(format!(".{name}.assembling-{suffix}")))
}

fn normalize_tree_time(root: &Path, epoch: i64) -> Result<(), ReleaseBundleError> {
    let time = FileTime::from_unix_time(epoch, 0);
    let mut paths = vec![root.to_path_buf()];
    let mut index = 0;
    while index < paths.len() {
        let path = paths[index].clone();
        index += 1;
        if path.is_dir() {
            for child in fs::read_dir(&path)? {
                paths.push(child?.path());
            }
        }
    }
    for path in paths.iter().rev() {
        filetime::set_file_mtime(path, time)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<(), ReleaseBundleError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<(), ReleaseBundleError> {
    Ok(())
}

#[cfg(unix)]
fn set_data_permissions(path: &Path) -> Result<(), ReleaseBundleError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_data_permissions(_path: &Path) -> Result<(), ReleaseBundleError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum ReleaseBundleError {
    #[error("release bundle input is invalid: {0}")]
    InvalidInput(&'static str),
    #[error("release bundle output path is invalid")]
    InvalidOutput,
    #[error("release bundle output already exists")]
    OutputExists,
    #[error("secure staging suffix generation is unavailable")]
    RandomUnavailable,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_bundle_is_complete_reproducible_and_packaged_unsupported() {
        let temporary = tempfile::tempdir().expect("release fixture");
        let input = temporary.path().join("input");
        fs::create_dir(&input).expect("input");
        let binary = |name: &str| {
            let path = input.join(name);
            fs::write(&path, format!("binary:{name}")).expect("binary");
            path
        };
        let ui = input.join("ui");
        fs::create_dir(&ui).expect("ui");
        fs::write(ui.join("index.html"), "<main>Aworkit</main>").expect("UI");
        let base = WholeBundleInputsV1 {
            output: temporary.path().join("bundle-a"),
            desktop: binary("aworkit-desktop"),
            trusted_core: binary("aworkit-trusted-core"),
            workflow_worker: binary("aworkit-workflow-worker"),
            capability_host: binary("aworkit-capability-host"),
            bootstrap_helper: binary("aworkit-bootstrap-helper"),
            ui_dist: ui,
            source_revision: "revision-12".to_owned(),
            source_tree_hash: "11".repeat(32),
            workspace_identity_hash: "22".repeat(32),
            toolchain_hash: "33".repeat(32),
            source_date_epoch: 1_700_000_000,
        };
        let first = assemble_whole_application_bundle(&base).expect("first bundle");
        let mut second_inputs = base.clone();
        second_inputs.output = temporary.path().join("bundle-b");
        let second = assemble_whole_application_bundle(&second_inputs).expect("second bundle");
        assert_eq!(first, second);
        assert_eq!(first.entries.len(), 6);
        assert_eq!(
            first.managed_local_self_activation,
            ActivationEligibilityV1::PackagedDistribution
        );
        assert!(matches!(
            first.build_origin,
            BuildOriginV1::PackagedDistribution { .. }
        ));
        assert_eq!(
            fs::read(base.output.join("WholeApplicationBundleV1.json")).expect("manifest"),
            fs::read(second_inputs.output.join("WholeApplicationBundleV1.json")).expect("manifest")
        );
    }
}
