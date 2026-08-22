//! Golden fixture tests keep Rust serialization compatible with the UI parser.
use aworkit_protocol::{
    BaseCommand, BaseError, BaseEvent, BaseRequest, BaseResult, Envelope, EnvelopeKind,
    ProcessGeneration, SchemaVersion, StableId,
};

fn fixture(path: &str) -> &'static str {
    match path {
        "command" => include_str!("../../../fixtures/protocol/v1/command-envelope.json"),
        "event" => include_str!("../../../fixtures/protocol/v1/event-envelope.json"),
        "request" => include_str!("../../../fixtures/protocol/v1/request-envelope.json"),
        "result" => include_str!("../../../fixtures/protocol/v1/result-envelope.json"),
        "error" => include_str!("../../../fixtures/protocol/v1/error-envelope.json"),
        _ => unreachable!("known fixture"),
    }
    .trim_end()
}

#[test]
fn command_fixture_round_trips_without_reformatting() {
    let fixture = fixture("command");
    let parsed: Envelope<BaseCommand> =
        serde_json::from_str(fixture).expect("valid golden fixture");
    parsed.validate_typed().expect("supported typed envelope");
    assert_eq!(parsed.schema_version, SchemaVersion::V1);
    assert_eq!(parsed.kind, EnvelopeKind::Command);
    assert_eq!(parsed.generation, ProcessGeneration(7));
    assert_eq!(
        parsed.message_id,
        StableId::parse("msg_01").expect("stable ID")
    );
    assert_eq!(serde_json::to_string(&parsed).expect("serialize"), fixture);
}

#[test]
fn every_base_payload_family_round_trips_exactly() {
    let event: Envelope<BaseEvent> = serde_json::from_str(fixture("event")).expect("event");
    let request: Envelope<BaseRequest> = serde_json::from_str(fixture("request")).expect("request");
    let result: Envelope<BaseResult> = serde_json::from_str(fixture("result")).expect("result");
    let error: Envelope<BaseError> = serde_json::from_str(fixture("error")).expect("error");

    event.validate_typed().expect("event kind");
    request.validate_typed().expect("request kind");
    result.validate_typed().expect("result kind");
    error.validate_typed().expect("error kind");
    assert_eq!(
        serde_json::to_string(&event).expect("event JSON"),
        fixture("event")
    );
    assert_eq!(
        serde_json::to_string(&request).expect("request JSON"),
        fixture("request")
    );
    assert_eq!(
        serde_json::to_string(&result).expect("result JSON"),
        fixture("result")
    );
    assert_eq!(
        serde_json::to_string(&error).expect("error JSON"),
        fixture("error")
    );
}
