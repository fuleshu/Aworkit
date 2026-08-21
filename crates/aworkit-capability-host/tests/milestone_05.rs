use aworkit_capability_host::{AdapterDescriptor, AdapterRegistry, ApprovedInvocation, CapabilityHost, CapabilityKind, Redactor, StreamNormalizer};
use aworkit_protocol::{ProcessGeneration, StableId};
use serde_json::json;
fn id(value: &str) -> StableId { StableId::parse(value).expect("id") }
#[test]
fn rejects_generation_and_binding_drift_and_redacts_every_event() {
    let mut registry = AdapterRegistry::default();
    registry.register(AdapterDescriptor { capability_id: "tool.file.read".into(), version: "1".into(), kind: CapabilityKind::FileRead }).expect("register");
    let host = CapabilityHost::new(ProcessGeneration(2), registry);
    let invocation = ApprovedInvocation { invocation_id: id("invocation.1"), host_generation: ProcessGeneration(1), capability_id: "tool.file.read".into(), adapter_version: "1".into(), kind: CapabilityKind::FileRead, payload: json!({}) };
    assert!(host.admit(&invocation).is_err());
    let redactor = Redactor::new(vec!["top-secret".into()]);
    let mut normalizer = StreamNormalizer::default();
    let (_, event) = normalizer.event("token=top-secret", &redactor);
    assert!(!event.contains("top-secret"));
    assert!(!normalizer.outcome(id("invocation.2"), None).retry_safe);
}
