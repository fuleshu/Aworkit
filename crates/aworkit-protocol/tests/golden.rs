//! Golden fixture tests keep Rust serialization compatible with the UI parser.
use aworkit_protocol::{
    BaseCommand, Envelope, EnvelopeKind, ProcessGeneration, SchemaVersion, StableId,
    validate_envelope,
};

#[test]
fn command_fixture_round_trips_without_reformatting() {
    let fixture = include_str!("../../../fixtures/protocol/v1/command-envelope.json").trim_end();
    let parsed: Envelope<BaseCommand> =
        serde_json::from_str(fixture).expect("valid golden fixture");
    validate_envelope(&parsed).expect("supported version");
    assert_eq!(parsed.schema_version, SchemaVersion::V1);
    assert_eq!(parsed.kind, EnvelopeKind::Command);
    assert_eq!(parsed.generation, ProcessGeneration(7));
    assert_eq!(
        parsed.message_id,
        StableId::parse("msg_01").expect("stable ID")
    );
    assert_eq!(serde_json::to_string(&parsed).expect("serialize"), fixture);
}
