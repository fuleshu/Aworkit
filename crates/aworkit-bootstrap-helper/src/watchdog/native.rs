//! Native verification-only application-generation process adapter.

use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use aworkit_process::{
    filesystem::{AnchoredDirectory, AnchoredRelativePath},
    identity::ExecutableIdentityV1,
    ipc::{AuthenticatedLocalListener, AuthenticatedLocalStream},
    runtime::{NativeProcessRegistry, SanitizedProcessSpecV1},
};
use aworkit_protocol::{ProcessGeneration, StableId};
use serde::{Serialize, de::DeserializeOwned};

use crate::journal::canonical_hash;

use super::{
    FocusedVerificationResultV1, GenerationHandshakeV1, GenerationHealthV1, LaunchObservationV1,
    PlatformLaunchRequestV1, PlatformProcessPortV1, ProcessTreeCleanupV1, ProcessTreeHandleV1,
};

const MAX_CONTROL_FRAME_BYTES: usize = 2 * 1024 * 1024;

struct LaunchContext {
    request: PlatformLaunchRequestV1,
    listener: Option<AuthenticatedLocalListener>,
    stream: Option<AuthenticatedLocalStream>,
    tree: ProcessTreeHandleV1,
}

/// Uses a helper-controlled managed root, cross-platform local sockets, and
/// retained Job/process-group handles for native bootstrap launches.
pub struct NativeBootstrapProcessPort {
    managed_root: AnchoredDirectory,
    registry: NativeProcessRegistry,
    launches: Mutex<HashMap<u64, LaunchContext>>,
    started: Instant,
}

impl NativeBootstrapProcessPort {
    pub fn open(managed_root: impl AsRef<Path>) -> Result<Self, String> {
        let managed_root =
            AnchoredDirectory::open(managed_root).map_err(|error| error.to_string())?;
        if !managed_root
            .capability_report()
            .supports_managed_publication()
        {
            return Err("managed root lacks native filesystem guarantees".to_owned());
        }
        Ok(Self {
            managed_root,
            registry: NativeProcessRegistry::default(),
            launches: Mutex::new(HashMap::new()),
            started: Instant::now(),
        })
    }

    fn executable_path(&self, request: &PlatformLaunchRequestV1) -> Result<PathBuf, String> {
        let digest = request
            .slot_handle
            .build_content_hash
            .strip_prefix("sha256:")
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| "invalid slot build hash".to_owned())?;
        let relative = AnchoredRelativePath::parse(format!(
            ".managed/slots/{digest}/{}",
            request.exact_core_entry
        ))
        .map_err(|error| error.to_string())?;
        self.managed_root
            .resolve_existing(&relative)
            .map_err(|error| error.to_string())
    }

    fn with_context<T>(
        &self,
        tree: &ProcessTreeHandleV1,
        operation: impl FnOnce(&mut LaunchContext) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut launches = self.launches.lock().expect("native bootstrap launch lock");
        let context = launches
            .get_mut(&tree.process_generation.0)
            .ok_or_else(|| "unknown bootstrap process generation".to_owned())?;
        if context.tree != *tree {
            return Err("bootstrap process-tree handle changed".to_owned());
        }
        operation(context)
    }
}

impl PlatformProcessPortV1 for NativeBootstrapProcessPort {
    fn request_cooperative_shutdown(&self, generation: ProcessGeneration) -> Result<(), String> {
        if let Some(context) = self
            .launches
            .lock()
            .expect("native bootstrap launch lock")
            .get_mut(&generation.0)
            && let Some(stream) = context.stream.as_mut()
        {
            write_frame(stream, &serde_json::json!({"kind": "cooperative_shutdown"}))?;
        }
        self.registry
            .request_cooperative_shutdown(generation)
            .map_err(|error| error.to_string())
    }

    fn await_tree_exit(
        &self,
        generation: ProcessGeneration,
        timeout_ms: u64,
    ) -> Result<bool, String> {
        self.registry
            .await_exit(generation, Duration::from_millis(timeout_ms))
            .map(|status| status.is_some())
            .map_err(|error| error.to_string())
    }

    fn force_terminate_tree(
        &self,
        generation: ProcessGeneration,
        timeout_ms: u64,
    ) -> Result<(), String> {
        let evidence = self
            .registry
            .force_cleanup(generation, Duration::from_millis(timeout_ms))
            .map_err(|error| error.to_string())?;
        if evidence.tree_empty {
            Ok(())
        } else {
            Err("native process tree could not be proven empty".to_owned())
        }
    }

    fn prove_tree_empty(
        &self,
        generation: ProcessGeneration,
    ) -> Result<ProcessTreeCleanupV1, String> {
        let evidence = self
            .registry
            .prove_empty(generation)
            .map_err(|error| error.to_string())?;
        let mut proof = ProcessTreeCleanupV1 {
            process_generation: generation,
            cooperative_requested: evidence.cooperative_requested,
            forced_termination_used: evidence.forced_termination_used,
            descendants_observed: u32::try_from(evidence.descendants_observed.len())
                .map_err(|_| "descendant census exceeds protocol range".to_owned())?,
            tree_empty: evidence.tree_empty,
            orphan_risk: evidence.orphan_risk,
            proof_hash: String::new(),
        };
        proof.proof_hash = canonical_hash(&proof).map_err(|error| error.to_string())?;
        Ok(proof)
    }

    fn spawn_verified(
        &self,
        request: &PlatformLaunchRequestV1,
    ) -> Result<LaunchObservationV1, String> {
        if !request.sanitized_environment
            || !request.inherited_handles_closed
            || !matches!(request.mode, super::BootstrapLaunchModeV1::VerificationOnly)
            || request.process_generation.0 == 0
        {
            return Err("unsafe or non-verification bootstrap launch denied".to_owned());
        }
        let executable_path = self.executable_path(request)?;
        let executable =
            ExecutableIdentityV1::open(&executable_path).map_err(|error| error.to_string())?;
        let listener = AuthenticatedLocalListener::bind(
            "bootstrap",
            request.process_generation,
            executable.content_hash.clone(),
        )
        .map_err(|error| error.to_string())?;
        let working_directory = executable_path
            .parent()
            .ok_or_else(|| "bootstrap executable has no parent".to_owned())?
            .to_path_buf();
        let spec = SanitizedProcessSpecV1 {
            executable: executable_path,
            arguments: vec![
                "--bootstrap-verification-only".to_owned(),
                "--aworkit-generation".to_owned(),
                request.process_generation.0.to_string(),
                "--aworkit-ipc".to_owned(),
                listener.address().as_str().to_owned(),
                "--aworkit-nonce".to_owned(),
                listener.nonce().to_owned(),
                "--aworkit-bundle-hash".to_owned(),
                request.slot_handle.build_content_hash.clone(),
            ],
            working_directory,
            environment: BTreeMap::new(),
            process_generation: request.process_generation,
            role: format!("bootstrap:{:?}", request.role),
            verification_only: true,
        };
        let native = self
            .registry
            .spawn_tree(&spec)
            .map_err(|error| error.to_string())?;
        let tree = ProcessTreeHandleV1 {
            handle_id: StableId::parse(format!(
                "bootstrap.native.tree.{}",
                request.process_generation.0
            ))
            .map_err(|error| error.to_string())?,
            process_generation: request.process_generation,
            root_process_identity_hash: native.executable.object_identity_hash.clone(),
            containment_identity_hash: native.containment_identity_hash,
        };
        let mut launch = LaunchObservationV1 {
            attempt_id: request.attempt_id.clone(),
            process_tree: tree.clone(),
            executable_hash: request.slot_handle.build_content_hash.clone(),
            slot_root_identity_hash: request.slot_handle.root_identity_hash.clone(),
            observed_at_monotonic_ms: self.started.elapsed().as_millis() as u64,
            observation_hash: String::new(),
        };
        launch.observation_hash = canonical_hash(&launch).map_err(|error| error.to_string())?;
        self.launches
            .lock()
            .expect("native bootstrap launch lock")
            .insert(
                request.process_generation.0,
                LaunchContext {
                    request: request.clone(),
                    listener: Some(listener),
                    stream: None,
                    tree,
                },
            );
        Ok(launch)
    }

    fn await_identity_handshake(
        &self,
        process_tree: &ProcessTreeHandleV1,
        timeout_ms: u64,
    ) -> Result<Option<GenerationHandshakeV1>, String> {
        self.with_context(process_tree, |context| {
            if context.stream.is_none() {
                let listener = context
                    .listener
                    .take()
                    .ok_or_else(|| "bootstrap listener was already consumed".to_owned())?;
                let stream = listener
                    .accept(Duration::from_millis(timeout_ms))
                    .map_err(|error| error.to_string())?;
                if !stream.peer.strong_executable_identity {
                    return Err("peer executable identity is not strong enough".to_owned());
                }
                context.stream = Some(stream);
            }
            let handshake = read_frame::<GenerationHandshakeV1>(
                context.stream.as_mut().expect("accepted bootstrap stream"),
            )?;
            Ok(Some(handshake))
        })
    }

    fn health_snapshot(
        &self,
        process_tree: &ProcessTreeHandleV1,
        timeout_ms: u64,
    ) -> Result<Option<GenerationHealthV1>, String> {
        self.with_context(process_tree, |context| {
            context
                .stream
                .as_ref()
                .ok_or_else(|| "bootstrap stream is not authenticated".to_owned())?
                .set_deadline(Duration::from_millis(timeout_ms))
                .map_err(|error| error.to_string())?;
            read_frame::<GenerationHealthV1>(
                context
                    .stream
                    .as_mut()
                    .ok_or_else(|| "bootstrap stream is not authenticated".to_owned())?,
            )
            .map(Some)
        })
    }

    fn handoff_focused_verification(
        &self,
        process_tree: &ProcessTreeHandleV1,
        verification_plan_hash: &str,
    ) -> Result<(), String> {
        self.with_context(process_tree, |context| {
            if context.request.verification_plan_hash != verification_plan_hash {
                return Err("verification-plan hash changed".to_owned());
            }
            write_frame(
                context
                    .stream
                    .as_mut()
                    .ok_or_else(|| "bootstrap stream is not authenticated".to_owned())?,
                &serde_json::json!({"verificationPlanHash": verification_plan_hash}),
            )
        })
    }

    fn await_focused_verification(
        &self,
        process_tree: &ProcessTreeHandleV1,
        timeout_ms: u64,
    ) -> Result<Option<FocusedVerificationResultV1>, String> {
        self.with_context(process_tree, |context| {
            context
                .stream
                .as_ref()
                .ok_or_else(|| "bootstrap stream is not authenticated".to_owned())?
                .set_deadline(Duration::from_millis(timeout_ms))
                .map_err(|error| error.to_string())?;
            read_frame::<FocusedVerificationResultV1>(
                context
                    .stream
                    .as_mut()
                    .ok_or_else(|| "bootstrap stream is not authenticated".to_owned())?,
            )
            .map(Some)
        })
    }
}

fn read_frame<T: DeserializeOwned>(stream: &mut AuthenticatedLocalStream) -> Result<T, String> {
    let inner = stream;
    let mut length = [0_u8; 4];
    inner
        .read_exact(&mut length)
        .map_err(|error| error.to_string())?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_CONTROL_FRAME_BYTES {
        return Err("bootstrap control frame exceeds its bound".to_owned());
    }
    let mut bytes = vec![0_u8; length];
    inner
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn write_frame<T: Serialize>(
    stream: &mut AuthenticatedLocalStream,
    value: &T,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_CONTROL_FRAME_BYTES {
        return Err("bootstrap control frame exceeds its bound".to_owned());
    }
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .map_err(|error| error.to_string())?;
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|error| error.to_string())
}
