//! Inert discovery of external tool plugins. Executables speak MCP over stdio;
//! services speak MCP over streamable HTTP. Discovery never starts either.

use super::McpToolConfiguration;
use crate::runtime::settings_v2::{IntegrationTransportV2, McpServerConfigurationV2};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path},
};

const MAX_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolPluginPin {
    pub manifest_path: String,
    pub content_hash: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveredToolPlugin {
    pub path: String,
    pub server: Option<McpServerConfigurationV2>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u16,
    id: String,
    name: String,
    version: String,
    execution: IntegrationTransportV2,
    #[serde(default)]
    tools: Vec<McpToolConfiguration>,
}

fn read_manifest(path: &Path) -> Result<(Manifest, ToolPluginPin), String> {
    for parent in path.ancestors() {
        let metadata = fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("Plugin path must not contain symbolic links".into());
        }
    }
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("Plugin manifest must be a file of at most 256 KiB".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Plugin manifest exceeds 256 KiB".into());
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    aworkit_protocol::StableId::parse(manifest.id.clone()).map_err(|e| e.to_string())?;
    if manifest.schema_version != 1
        || manifest.name.trim().is_empty()
        || manifest.name.len() > 256
        || manifest.version.is_empty()
        || manifest.version.len() > 128
    {
        return Err("Unsupported tool plugin identity or version".into());
    }
    super::validate_mcp_catalog(&manifest.tools)?;
    let pin = ToolPluginPin {
        manifest_path: fs::canonicalize(path)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned(),
        content_hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
        version: manifest.version.clone(),
    };
    Ok((manifest, pin))
}

/// Resolves package-relative executable and working-directory paths explicitly.
fn package_path(base: &Path, value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Ok(value.into());
    }
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err("Relative plugin paths must stay within the plugin directory".into());
    }
    Ok(base.join(path).to_string_lossy().into_owned())
}

pub fn inspect(path: &Path) -> Result<McpServerConfigurationV2, String> {
    let (mut manifest, pin) = read_manifest(path)?;
    let base = Path::new(&pin.manifest_path)
        .parent()
        .ok_or("Plugin directory is missing")?;
    if let IntegrationTransportV2::Stdio { command, cwd, .. } = &mut manifest.execution {
        *command = package_path(base, command)?;
        *cwd = Some(match cwd {
            Some(value) => package_path(base, value)?,
            None => base.to_string_lossy().into_owned(),
        });
    }
    Ok(McpServerConfigurationV2 {
        id: manifest.id,
        name: manifest.name,
        enabled: false,
        auto_connect: false,
        transport: manifest.execution,
        tools: manifest.tools,
        plugin: Some(pin),
    })
}

pub fn verify(pin: &ToolPluginPin) -> Result<(), String> {
    let (_, actual) = read_manifest(Path::new(&pin.manifest_path))?;
    if actual != *pin {
        return Err(
            "Tool plugin manifest changed. Review and re-add its new version in Settings.".into(),
        );
    }
    Ok(())
}

/// One level of bounded folder discovery; malformed packages remain inspectable.
pub fn discover(root: &Path) -> Vec<DiscoveredToolPlugin> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            return vec![DiscoveredToolPlugin {
                path: root.to_string_lossy().into_owned(),
                server: None,
                error: Some(e.to_string()),
            }];
        }
    };
    let mut paths = entries
        .take(257)
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("tool-plugin.json"))
        .collect::<Vec<_>>();
    paths.sort();
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .map(|path| {
            let result = inspect(&path).and_then(|server| {
                if seen.insert(server.id.clone()) {
                    Ok(server)
                } else {
                    Err(format!("Duplicate tool plugin id '{}'", server.id))
                }
            });
            match result {
                Ok(server) => DiscoveredToolPlugin {
                    path: path.to_string_lossy().into_owned(),
                    server: Some(server),
                    error: None,
                },
                Err(error) => DiscoveredToolPlugin {
                    path: path.to_string_lossy().into_owned(),
                    server: None,
                    error: Some(error),
                },
            }
        })
        .collect()
}
