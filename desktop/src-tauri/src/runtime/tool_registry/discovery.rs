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
/// One installed package is bounded so a folder copy can never exhaust the disk
/// or run for an unbounded time.
const MAX_PACKAGE_FILES: usize = 4_096;
const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

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
        return Err("A plugin file must be at most 256 KiB".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("A plugin file is larger than 256 KiB".into());
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

/// Resolves a package-relative executable path.
///
/// A bare command name with no path separator is only a name, so it is left for
/// the operating system to resolve from `PATH` (for example `python` or
/// `node`). A command with a separator, or `./name`, is package-relative and
/// must stay inside the plugin folder. An absolute command selects an installed
/// interpreter or executable directly.
fn package_command(base: &Path, value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Ok(value.into());
    }
    let mut components = path.components();
    if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
        return Ok(value.into());
    }
    package_relative(base, value)
}

/// Resolves a package-relative working directory inside the plugin folder.
fn package_relative(base: &Path, value: &str) -> Result<String, String> {
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
        *command = package_command(base, command)?;
        *cwd = Some(match cwd {
            Some(value) => package_relative(base, value)?,
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
            "This plugin changed. Refresh the plugins list, then add its new version in Settings."
                .into(),
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

/// The canonical folder the discovery scan reads. Plugins merely copied here
/// are sourced, listed and disabled until the user enables one.
pub fn plugin_folder(root: &Path) -> Result<std::path::PathBuf, String> {
    if root.as_os_str().is_empty() {
        return Err("Tool plugin folder is not configured".into());
    }
    fs::create_dir_all(root).map_err(|e| format!("could not create the plugin folder: {e}"))?;
    fs::canonicalize(root).map_err(|e| format!("could not resolve the plugin folder: {e}"))
}

/// Where one installed package would live: `<root>/<folder>` with a single,
/// safe folder name so an install can never escape the plugin folder.
fn package_destination(root: &Path, folder: &str) -> Result<std::path::PathBuf, String> {
    let candidate = Path::new(folder);
    if folder.is_empty()
        || folder.len() > 128
        || folder.starts_with('.')
        || candidate.components().count() != 1
        || !matches!(candidate.components().next(), Some(Component::Normal(_)))
    {
        return Err("the plugin folder name is not usable".into());
    }
    Ok(root.join(folder))
}

/// Finds the package folder that directly contains `tool-plugin.json`.
///
/// The user may choose either the package folder itself or a parent that holds
/// exactly one package subfolder, which is what a downloaded archive unpacks to.
fn package_source(source: &Path) -> Result<std::path::PathBuf, String> {
    if !source.is_dir() {
        return Err("Choose the folder that contains tool-plugin.json.".into());
    }
    if source.join("tool-plugin.json").is_file() {
        return Ok(source.to_path_buf());
    }
    let mut candidates = fs::read_dir(source)
        .map_err(|e| e.to_string())?
        .take(64)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("tool-plugin.json").is_file());
    let first = candidates.next();
    if first.is_some() && candidates.next().is_none() {
        return first.ok_or_else(|| "the plugin folder is empty".to_owned());
    }
    Err("Choose a folder that contains tool-plugin.json (one plugin per folder).".into())
}

/// Copies one package folder into the plugin folder and reports the discovered
/// plugin. Existing code is never executed, and a package that is already
/// installed is replaced by the new copy.
fn copy_package(source: &Path, destination: &Path) -> Result<(), String> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![(source.to_path_buf(), destination.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        fs::create_dir_all(&to).map_err(|e| e.to_string())?;
        let entries = fs::read_dir(&from).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let file_type = entry.file_type().map_err(|e| e.to_string())?;
            if file_type.is_symlink() {
                return Err("A plugin package must not contain symbolic links.".into());
            }
            let name = entry.file_name();
            let next = to.join(&name);
            if file_type.is_dir() {
                stack.push((entry.path(), next));
            } else if file_type.is_file() {
                files += 1;
                let size = entry.metadata().map_err(|e| e.to_string())?.len();
                bytes = bytes.saturating_add(size);
                if files > MAX_PACKAGE_FILES || bytes > MAX_PACKAGE_BYTES {
                    return Err("The plugin folder is larger than Aworkit will install.".into());
                }
                fs::copy(entry.path(), &next).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// Installs one chosen package folder into the plugin folder.
///
/// The copy is inert: nothing is executed, nothing is enabled, and the result
/// is reported as a newly sourced plugin that the user still has to enable.
pub fn install(source: &Path, root: &Path) -> Result<McpServerConfigurationV2, String> {
    let root = plugin_folder(root)?;
    let source = package_source(source)?;
    let source = fs::canonicalize(&source).map_err(|e| e.to_string())?;
    if source.starts_with(&root) {
        return Err("That plugin is already in the plugin folder.".into());
    }
    let folder = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("The chosen folder has no usable name")?;
    let destination = package_destination(&root, folder)?;
    // Stage inside the plugin folder first so a failed copy never leaves a
    // half-written package that discovery would then read.
    let staging = root.join(format!(".installing-{folder}"));
    let _ = fs::remove_dir_all(&staging);
    if let Err(error) = copy_package(&source, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let inspected = inspect(&staging.join("tool-plugin.json"));
    match inspected {
        Ok(_) => {
            if destination.exists() {
                fs::remove_dir_all(&destination)
                    .map_err(|e| format!("could not replace the installed plugin: {e}"))?;
            }
            fs::rename(&staging, &destination)
                .map_err(|e| format!("could not finish installing the plugin: {e}"))?;
            // Report the plugin from its final location, with the package
            // folder as the resolved working directory.
            inspect(&destination.join("tool-plugin.json"))
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

/// Removes one sourced plugin package from the plugin folder.
///
/// `key` is either a discovered plugin id or the manifest path the plugin list
/// showed. Removing the files is all this does; the caller decides what the
/// saved workflow configuration does about the missing server.
pub fn remove(root: &Path, key: &str) -> Result<String, String> {
    let root = plugin_folder(root)?;
    let path = if Path::new(key).is_absolute() {
        Path::new(key).to_path_buf()
    } else {
        let discovered = discover(&root);
        discovered
            .iter()
            .find(|entry| entry.server.as_ref().is_some_and(|server| server.id == key))
            .map(|entry| Path::new(&entry.path).to_path_buf())
            .ok_or_else(|| format!("The plugin '{key}' is not in the plugin folder."))?
    };
    let folder = path
        .parent()
        .ok_or("The plugin folder is missing")?
        .to_path_buf();
    let canonical = fs::canonicalize(&folder).map_err(|e| e.to_string())?;
    if canonical == root || !canonical.starts_with(&root) {
        return Err("That folder is not an installed plugin.".into());
    }
    fs::remove_dir_all(&canonical).map_err(|e| format!("could not remove the plugin: {e}"))?;
    Ok(canonical.to_string_lossy().into_owned())
}
