use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind { Shell, Python, Model, FileRead, FileSearch, FileEdit }
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdapterDescriptor { pub capability_id: String, pub version: String, pub kind: CapabilityKind }
#[derive(Clone, Default)]
pub struct AdapterRegistry { descriptors: BTreeMap<String, AdapterDescriptor> }
impl AdapterRegistry { pub fn register(&mut self, descriptor: AdapterDescriptor) -> Result<(), RegistryError> { if self.descriptors.contains_key(&descriptor.capability_id) { return Err(RegistryError::Duplicate); } self.descriptors.insert(descriptor.capability_id.clone(), descriptor); Ok(()) } pub fn resolve(&self, id: &str, version: &str) -> Result<&AdapterDescriptor, RegistryError> { let value = self.descriptors.get(id).ok_or(RegistryError::Unknown)?; if value.version != version { return Err(RegistryError::VersionDrift); } Ok(value) } }
#[derive(Debug, Error, Eq, PartialEq)]
pub enum RegistryError { #[error("duplicate adapter descriptor")] Duplicate, #[error("unknown approved adapter")] Unknown, #[error("adapter version drift")] VersionDrift }
