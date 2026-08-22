use std::sync::atomic::{AtomicUsize, Ordering};

use aworkit_capability_host::{
    AdapterRegistry, AdmissionReceipt, AdmittedInvocationDispatcherV1,
    ApprovedInvocationEnvelopeV1, CapabilityDescriptor, CapabilityHost, CapabilityKind,
    DispatchLifecycleV1, FrozenAdapterRegistry, HostError, RegistryError, SideEffectClass,
    build_extension_handshake_v1,
};
use aworkit_protocol::{
    AttestedExtensionSetV1, AttestedExtensionV1, CapabilityDescriptorV1, CapabilityKindV1,
    CapabilitySideEffectV1, CapabilityVisibilityV1, ExtensionContributionV1, ExtensionIdentityV1,
    ExtensionRuntimeBindingV1, ProcessGeneration, SchemaVersion, StableId,
    attested_extension_set_hash_v1, capability_descriptor_hash_v1,
};
use serde_json::json;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable test ID")
}

fn protocol_descriptor() -> CapabilityDescriptorV1 {
    let mut descriptor = CapabilityDescriptorV1 {
        capability_id: id("extension.tool"),
        adapter_version: "1.0.0".into(),
        kind: CapabilityKindV1::Plugin,
        side_effect: CapabilitySideEffectV1::Unknown,
        guarantees_same_id_deduplication: false,
        supports_streaming: true,
        supports_cancellation: true,
        supports_continuation: false,
        supports_sessions: true,
        supports_approval_forwarding: true,
        supports_mcp_forwarding: false,
        allowed_scopes: vec!["project.read".into()],
        secret_slots: Vec::new(),
        input_schema_hash: None,
        output_schema_hash: None,
        requires_workspace: true,
        required_isolation: Some("isolation.strict.v1".into()),
        maximum_concurrency: 1,
        max_input_bytes: 4096,
        max_output_bytes: 8192,
        supported_platforms: vec!["linux".into()],
        visibility: CapabilityVisibilityV1::TrustedDesktopUser,
        descriptor_hash: String::new(),
    };
    descriptor.descriptor_hash =
        capability_descriptor_hash_v1(&descriptor).expect("descriptor hash");
    descriptor
}

fn attested_set(
    generation: ProcessGeneration,
) -> (AttestedExtensionSetV1, ExtensionContributionV1) {
    let identity = ExtensionIdentityV1 {
        extension_id: id("extension.example"),
        version: "1.0.0".into(),
        content_hash: format!("sha256:{}", "a".repeat(64)),
    };
    let contribution = ExtensionContributionV1 {
        contribution_id: id("extension.example.primary"),
        descriptor: protocol_descriptor(),
    };
    let handshake = build_extension_handshake_v1(
        id("host.primary"),
        generation,
        identity.clone(),
        "bin/example".into(),
        true,
        vec![contribution.clone()],
    )
    .expect("handshake");
    let mut set = AttestedExtensionSetV1 {
        host_id: id("host.primary"),
        host_generation: generation,
        host_protocol: 1,
        extensions: vec![AttestedExtensionV1 {
            identity,
            handshake_hash: handshake.handshake_hash,
            contributions: vec![contribution.clone()],
        }],
        set_hash: String::new(),
    };
    set.set_hash = attested_extension_set_hash_v1(&set).expect("set hash");
    (set, contribution)
}

fn frozen_registry(set: &AttestedExtensionSetV1) -> FrozenAdapterRegistry {
    AdapterRegistry::default()
        .materialize_attested_set(set)
        .expect("frozen attested registry")
}

fn extension_binding(set: &AttestedExtensionSetV1) -> ExtensionRuntimeBindingV1 {
    let extension = &set.extensions[0];
    ExtensionRuntimeBindingV1 {
        identity: extension.identity.clone(),
        contribution_id: extension.contributions[0].contribution_id.clone(),
        host_id: set.host_id.clone(),
        host_generation: set.host_generation,
        handshake_hash: extension.handshake_hash.clone(),
    }
}

fn approved_envelope(
    set: &AttestedExtensionSetV1,
    contribution: &ExtensionContributionV1,
    invocation_id: &str,
) -> ApprovedInvocationEnvelopeV1 {
    let descriptor = &contribution.descriptor;
    let mut envelope = ApprovedInvocationEnvelopeV1 {
        schema_version: SchemaVersion::V1,
        invocation_id: id(invocation_id),
        decision_id: id(&format!("decision.{invocation_id}")),
        host_generation: set.host_generation,
        capability_id: descriptor.capability_id.to_string(),
        adapter_version: descriptor.adapter_version.clone(),
        binding_hash: descriptor.descriptor_hash.clone(),
        extension: Some(extension_binding(set)),
        required_isolation_profile: descriptor.required_isolation.clone(),
        kind: CapabilityKind::Plugin,
        enforced_scopes: vec!["project.read".into()],
        deadline_epoch_millis: 10_000,
        cancellation_token: id(&format!("cancel.{invocation_id}")),
        lease_handles: Vec::new(),
        max_output_bytes: 1024,
        payload: json!({"operation": "inspect"}),
        core_authentication_tag: String::new(),
    };
    envelope.sign(b"core-key").expect("sign envelope");
    envelope
}

#[derive(Default)]
struct CountingDispatcher {
    calls: AtomicUsize,
}

impl AdmittedInvocationDispatcherV1 for CountingDispatcher {
    type Output = String;

    fn dispatch(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        admission: &AdmissionReceipt,
        _cancellation: &aworkit_capability_host::CancellationToken,
    ) -> Self::Output {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(admission.invocation_id, envelope.invocation_id);
        admission.descriptor.version_hash.clone()
    }
}

struct ActiveDispatcher;

impl AdmittedInvocationDispatcherV1 for ActiveDispatcher {
    type Output = ();

    fn dispatch(
        &self,
        _envelope: &ApprovedInvocationEnvelopeV1,
        _admission: &AdmissionReceipt,
        _cancellation: &aworkit_capability_host::CancellationToken,
    ) {
    }

    fn lifecycle(&self, _output: &Self::Output) -> DispatchLifecycleV1 {
        DispatchLifecycleV1::Active
    }
}

#[test]
fn core_attested_set_materializes_only_exact_generation_and_descriptors() {
    let (set, contribution) = attested_set(ProcessGeneration(7));

    let mut built_ins = AdapterRegistry::default();
    built_ins
        .register_capability(
            CapabilityDescriptor::build(
                "builtin.shell",
                "1.0.0",
                CapabilityKind::Shell,
                SideEffectClass::Unknown,
            )
            .expect("built-in descriptor"),
        )
        .expect("built-in");
    let frozen = built_ins
        .materialize_attested_set(&set)
        .expect("materialize exact set");
    assert_eq!(frozen.generation(), ProcessGeneration(7));
    assert_eq!(frozen.attested_set_hash(), Some(set.set_hash.as_str()));
    assert!(
        frozen
            .resolve_exact(
                contribution.descriptor.capability_id.as_str(),
                &contribution.descriptor.adapter_version,
                &contribution.descriptor.descriptor_hash,
            )
            .is_ok()
    );

    let mut drifted = set;
    drifted.host_generation = ProcessGeneration(8);
    assert!(matches!(
        AdapterRegistry::default().materialize_attested_set(&drifted),
        Err(RegistryError::AttestedSetHashDrift)
    ));
}

#[test]
fn production_dispatch_rejects_unattested_and_stale_extension_bindings_before_execution() {
    let (set, contribution) = attested_set(ProcessGeneration(7));
    let frozen = frozen_registry(&set);

    let unattested = AdapterRegistry::default().freeze(ProcessGeneration(7));
    assert!(matches!(
        CapabilityHost::from_attested_registry(unattested, b"core-key".to_vec(), 4),
        Err(HostError::UnattestedRegistry)
    ));
    assert!(matches!(
        CapabilityHost::from_attested_registry(frozen.clone(), Vec::new(), 4),
        Err(HostError::InvalidCoreKey)
    ));

    let host = CapabilityHost::from_attested_registry(frozen, b"core-key".to_vec(), 4)
        .expect("production host");
    let dispatcher = CountingDispatcher::default();
    let valid = approved_envelope(&set, &contribution, "invocation.valid");
    let dispatched = host
        .dispatch_v1(&valid, 9_000, &dispatcher)
        .expect("dispatch exact binding");
    assert_eq!(
        dispatched.output,
        Some(contribution.descriptor.descriptor_hash.clone())
    );
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);

    let duplicate = host
        .dispatch_v1(&valid, 9_000, &dispatcher)
        .expect("completed duplicate");
    assert!(duplicate.output.is_none());
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);

    let active = approved_envelope(&set, &contribution, "invocation.active");
    let active_receipt = host
        .dispatch_v1(&active, 9_000, &ActiveDispatcher)
        .expect("start long-lived admitted invocation");
    assert_eq!(active_receipt.lifecycle, Some(DispatchLifecycleV1::Active));
    let active_duplicate = host
        .dispatch_v1(&active, 9_000, &ActiveDispatcher)
        .expect("active duplicate");
    assert!(active_duplicate.output.is_none());
    host.complete(&active.invocation_id)
        .expect("explicit long-lived settlement");

    let mut missing_attestation =
        approved_envelope(&set, &contribution, "invocation.missing-attestation");
    missing_attestation.extension = None;
    missing_attestation.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.dispatch_v1(&missing_attestation, 9_000, &dispatcher),
        Err(HostError::Registry(
            RegistryError::ExtensionAttestationDrift
        ))
    ));

    let mut stale_code = approved_envelope(&set, &contribution, "invocation.stale-code");
    stale_code
        .extension
        .as_mut()
        .expect("extension")
        .identity
        .content_hash = format!("sha256:{}", "b".repeat(64));
    stale_code.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.dispatch_v1(&stale_code, 9_000, &dispatcher),
        Err(HostError::Registry(
            RegistryError::ExtensionAttestationDrift
        ))
    ));

    let mut stale_handshake = approved_envelope(&set, &contribution, "invocation.stale-handshake");
    stale_handshake
        .extension
        .as_mut()
        .expect("extension")
        .handshake_hash = format!("sha256:{}", "c".repeat(64));
    stale_handshake.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.dispatch_v1(&stale_handshake, 9_000, &dispatcher),
        Err(HostError::Registry(
            RegistryError::ExtensionAttestationDrift
        ))
    ));

    let mut stale_isolation = approved_envelope(&set, &contribution, "invocation.stale-isolation");
    stale_isolation.required_isolation_profile = Some("isolation.weaker.v1".into());
    stale_isolation.sign(b"core-key").expect("resign");
    assert!(matches!(
        host.dispatch_v1(&stale_isolation, 9_000, &dispatcher),
        Err(HostError::Registry(RegistryError::IsolationProfileDrift))
    ));
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
}
