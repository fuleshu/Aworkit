//! Compatibility regressions for stored frozen Chat records (task 184, P0).
//!
//! A profile outlives the build that wrote it. Every test here replays stored
//! bytes exactly as a previous or a later build wrote them - hand-written JSON
//! whose every hash is the canonical hash of the bytes actually stored - and
//! asserts that this build still reads the record, still verifies each hash
//! against those bytes, and still refuses a tampered value.
use super::*;

use crate::runtime::settings_v2::{ModelTargetV2, ModelTierKindV2, ModelTierResolutionV2};

const FIXTURE_CHAT_ID: &str = "chat.compat-fixture";
const FIXTURE_RUN_ID: &str = "run.compat-fixture";

fn hash_of(value: &Value) -> String {
    canonical_hash(value).expect("canonical hash")
}

fn fixture_provider() -> ProviderConfigurationV2 {
    ProviderConfigurationV2 {
        id: "provider.fixture".into(),
        name: "Fixture provider".into(),
        kind: "openai_compatible".into(),
        base_url: "http://127.0.0.1:9999/v1".into(),
        enabled: true,
        credential_ref: None,
        models: vec![fixture_model()],
        configuration: BTreeMap::new(),
    }
}

fn fixture_model() -> ModelConfigurationV2 {
    ModelConfigurationV2 {
        id: "model.fixture".into(),
        name: "Fixture model".into(),
        remote_id: "fixture-model".into(),
        enabled: true,
        context_window: Some(32_768),
        max_output_tokens: None,
        compaction: None,
        capabilities: vec!["text".into()],
        parameters: BTreeMap::new(),
    }
}

/// One complete, self-consistent frozen execution context: every stored hash is
/// the canonical hash of the value it names.
fn fixture_context() -> FrozenChatExecutionContextV1 {
    let workflow_snapshot = json!({"schemaVersion": 1, "nodes": []});
    let provider = fixture_provider();
    let model = fixture_model();
    let tier = ModelTierConfigurationV2 {
        id: "tier:balanced".into(),
        name: "Balanced".into(),
        kind: ModelTierKindV2::Standard,
        resolution: ModelTierResolutionV2::Exact {
            target: ModelTargetV2 {
                provider_id: provider.id.clone(),
                model_id: model.id.clone(),
            },
        },
    };
    FrozenChatExecutionContextV1 {
        compaction_version: None,
        summary_target: None,
        mcp_configurations: Vec::new(),
        approval_mode: None,
        schema_version: 1,
        identity: ChatIdentityV1 {
            chat_id: StableId::parse(FIXTURE_CHAT_ID.to_owned()).unwrap(),
            run_id: StableId::parse(FIXTURE_RUN_ID.to_owned()).unwrap(),
        },
        history_base_head: 0,
        start_command_id: StableId::parse("chat.compat-start".to_owned()).unwrap(),
        start_command_hash: format!("sha256:{}", "a".repeat(64)),
        pending_start_command: None,
        settings_version: 1,
        project: None,
        chat_workspace: None,
        workflow_id: "workflow.simple-chat".into(),
        workflow_name: "Simple Chat".into(),
        workflow_version: 1,
        workflow_snapshot_hash: canonical_hash(&workflow_snapshot).unwrap(),
        workflow_snapshot,
        legacy_agent_maximum_turns: None,
        legacy_maximum_tool_calls: None,
        run_deadline_millis: 60_000,
        tools: Vec::new(),
        model_tier_id: tier.id.clone(),
        model_tier_hash: canonical_hash(&tier).unwrap(),
        model_tier_snapshot: tier,
        provider_id: provider.id.clone(),
        provider_name: provider.name.clone(),
        provider_kind: provider.kind.clone(),
        provider_base_url: provider.base_url.clone(),
        provider_hash: canonical_hash(&provider).unwrap(),
        provider_snapshot: provider,
        model_id: model.id.clone(),
        model_name: model.name.clone(),
        remote_model_id: model.remote_id.clone(),
        model_hash: canonical_hash(&model).unwrap(),
        model_snapshot: model,
        credential: None,
        mcp_manifests: BTreeMap::new(),
    }
}

/// The stored record exactly as this build writes it.
fn stored_record(context: FrozenChatExecutionContextV1) -> Value {
    let value = serde_json::to_value(&context).unwrap();
    json!({"context": value, "contextHash": hash_of(&value)})
}

/// The same record with a rewritten stored context, re-hashed over the bytes
/// that were actually stored: this is what a different build's writer produced.
fn stored_record_from_bytes(mut context: Value, rewrite: impl FnOnce(&mut Value)) -> Value {
    rewrite(&mut context);
    json!({"context": context, "contextHash": hash_of(&context)})
}

/// A frozen tool binding whose stored tool hash covers its stored snapshot.
fn tool_binding(future_field: bool) -> Value {
    let mut snapshot = json!({
        "id": "tool.files.read",
        "name": "Read file",
        "enabled": true,
        "requiresProject": false,
        "credentialBindings": [],
        "configuration": {
            "authorityMode": "project_files",
            "effect": "read",
            "maximumBytes": 65536,
        },
    });
    if future_field {
        snapshot["futureToolField"] = json!("added by a later build");
    }
    let tool_hash = hash_of(&snapshot);
    json!({"toolId": "tool.files.read", "toolHash": tool_hash, "toolSnapshot": snapshot})
}

#[test]
fn the_current_schema_version_is_validated_with_the_current_rules() {
    let record = decode_stored_frozen_context_record(stored_record(fixture_context()))
        .expect("a record written by this build loads");
    assert_eq!(record.context.schema_version, FROZEN_RECORD_SCHEMA_VERSION);
    assert_eq!(record.context.read_only_notice(), None);
    assert_eq!(
        frozen_record_admission(FROZEN_RECORD_SCHEMA_VERSION).unwrap(),
        StoredFrozenRecordAdmission::Current
    );
}

#[test]
fn a_newer_frozen_record_schema_opens_read_only_instead_of_failing_the_read() {
    // A later build froze this Chat with a schema this build does not know and
    // fields this build has never seen. Its writer hashed the bytes it stored.
    let value = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context["schemaVersion"] = json!(2);
            context["futureSessionField"] = json!("written by a newer build");
        },
    );
    let record = decode_stored_frozen_context_record(value)
        .expect("a record from a newer build must still be readable");
    assert_eq!(record.context.schema_version, 2);
    assert_eq!(
        frozen_record_admission(2).unwrap(),
        StoredFrozenRecordAdmission::ReadOnlyNewer { version: 2 }
    );
    let notice = record
        .context
        .read_only_notice()
        .expect("a newer record opens read-only");
    assert!(notice.contains("newer Aworkit"), "{notice}");
    assert!(notice.contains("read-only"), "{notice}");
    assert!(notice.contains("schema version 2"), "{notice}");
}

#[test]
fn a_newer_frozen_record_is_still_verified_byte_exactly() {
    // The version gate never weakens tamper detection: a newer record whose
    // stored bytes changed is still refused.
    let mut tampered = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context["schemaVersion"] = json!(2);
        },
    );
    tampered["context"]["workflowName"] = json!("Renamed in place");
    assert!(
        decode_stored_frozen_context_record(tampered).is_err(),
        "a tampered newer record must stay rejected"
    );

    // A nested value changed while the outer stored hash stays as written.
    let mut nested = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context["schemaVersion"] = json!(2);
        },
    );
    nested["context"]["providerSnapshot"]["baseUrl"] = json!("http://tampered.invalid/v1");
    assert!(decode_stored_frozen_context_record(nested).is_err());
}

#[test]
fn a_version_below_the_first_released_one_names_its_missing_migration_path() {
    let value = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context["schemaVersion"] = json!(0);
        },
    );
    let error = decode_stored_frozen_context_record(value)
        .expect_err("a version no build ever wrote is damage, not evidence");
    assert!(error.contains("no migration path"), "{error}");
    assert!(error.contains("version 0"), "{error}");
}

#[test]
fn a_record_without_a_usable_schema_version_is_named_as_such() {
    let value = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context.as_object_mut().unwrap().remove("schemaVersion");
        },
    );
    let error = decode_stored_frozen_context_record(value)
        .expect_err("a record with no schema version cannot select a rule set");
    assert!(error.contains("no supported schema version"), "{error}");
}

#[test]
fn a_record_this_build_writes_still_decodes() {
    let record = decode_stored_frozen_context_record(stored_record(fixture_context()))
        .expect("a record written by this build loads");
    assert_eq!(record.context.identity.chat_id.as_str(), FIXTURE_CHAT_ID);
}

#[test]
fn stored_bytes_from_another_build_load_with_every_nested_hash_verified() {
    // A later build added a field to the frozen tool snapshot. Its writer hashed
    // the bytes it stored, so this build must verify the stored value instead of
    // re-deriving the snapshot from today's struct.
    let value = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context["tools"] = json!([tool_binding(true)]);
        },
    );
    let record = decode_stored_frozen_context_record(value)
        .expect("an unknown key inside a frozen tool snapshot must not lock a Chat out");
    assert_eq!(record.context.tools.len(), 1);
    assert_eq!(record.context.tools[0].tool_id, "tool.files.read");
    assert!(record.context.tools[0]
        .tool_snapshot
        .configuration
        .contains_key("authorityMode"));

    // The same for the frozen context itself: an unknown top-level key is
    // dropped and the stored context hash still verifies.
    let value = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context["futureContextField"] = json!("added by a later build");
        },
    );
    let record = decode_stored_frozen_context_record(value)
        .expect("an unknown key in a frozen context must not lock a Chat out");
    assert_eq!(record.context.identity.chat_id.as_str(), FIXTURE_CHAT_ID);
}

#[test]
fn a_record_missing_a_field_added_later_still_loads() {
    // A record written before `runDeadlineMillis` existed carries no such field;
    // the reader supplies the default and still verifies the stored bytes.
    let value = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context.as_object_mut().unwrap().remove("runDeadlineMillis");
        },
    );
    let record = decode_stored_frozen_context_record(value)
        .expect("a record missing a field added later must still load");
    assert_eq!(
        record.context.run_deadline_millis,
        default_run_deadline_millis()
    );
}

#[test]
fn a_record_carrying_only_a_read_side_compatibility_field_still_loads() {
    // `agentMaximumTurns` is read from stored records and never written again,
    // so a record that carries it re-serializes differently from its own bytes.
    let value = stored_record_from_bytes(
        serde_json::to_value(fixture_context()).unwrap(),
        |context| {
            context["agentMaximumTurns"] = json!(24);
        },
    );
    let record = decode_stored_frozen_context_record(value)
        .expect("a stored compatibility field must not lock a Chat out");
    assert_eq!(record.context.legacy_agent_maximum_turns, Some(24));
}

#[test]
fn a_tampered_stored_value_is_still_rejected() {
    type Rewrite = fn(&mut Value);

    // A nested snapshot changed while its own stored hash stays as written.
    let tampered_snapshot: Rewrite = |context| {
        context["providerSnapshot"]["configuration"]["injectedByTamper"] = json!(true);
    };
    // A nested hash replaced with a different, well-formed hash.
    let tampered_nested_hash: Rewrite = |context| {
        context["providerHash"] = json!(format!("sha256:{}", "b".repeat(64)));
    };
    // A frozen tool snapshot changed while its stored tool hash stays as written.
    let tampered_tool: Rewrite = |context| {
        context["tools"] = json!([tool_binding(false)]);
        let tool_hash = context["tools"][0]["toolHash"].clone();
        context["tools"][0]["toolSnapshot"]["configuration"]["maximumBytes"] = json!(1);
        context["tools"][0]["toolHash"] = tool_hash;
    };

    for rewrite in [tampered_snapshot, tampered_nested_hash, tampered_tool] {
        let value =
            stored_record_from_bytes(serde_json::to_value(fixture_context()).unwrap(), rewrite);
        assert!(
            decode_stored_frozen_context_record(value.clone()).is_err(),
            "a tampered stored value must stay rejected"
        );
    }

    // The stored context itself changed while its stored hash stays as written.
    let mut tampered = stored_record(fixture_context());
    tampered["context"]["workflowName"] = json!("Renamed in place");
    assert!(decode_stored_frozen_context_record(tampered).is_err());
}

#[test]
fn one_unreadable_record_does_not_break_reads_for_other_chats() {
    let root = TempDir::new().unwrap();
    let port = Arc::new(SwitchableEventPort {
        fail: AtomicBool::new(false),
        delivered: Mutex::new(Vec::new()),
    });
    let history = ChatHistory::open_with_committed_events(root.path(), port).unwrap();

    // A record this build cannot read, written for an unrelated Chat, staged
    // first so an exhaustive scan would reach it before the healthy record.
    history
        .stage_stored_session_event_for_test(
            "chat.execution-context-frozen",
            json!({
                "schemaVersion": 1,
                "record": {
                    "context": {
                        "schemaVersion": 1,
                        "identity": {"chatId": "chat.unreadable", "runId": "run.unreadable"},
                    },
                    "contextHash": format!("sha256:{}", "c".repeat(64)),
                },
            }),
        )
        .unwrap();
    history
        .stage_stored_session_event_for_test(
            "chat.execution-context-frozen",
            json!({"schemaVersion": 1, "record": stored_record(fixture_context())}),
        )
        .unwrap();

    let healthy = StableId::parse(FIXTURE_CHAT_ID.to_owned()).unwrap();
    let readable = history
        .frozen_context(&healthy)
        .unwrap()
        .expect("an unreadable unrelated record must not block this Chat");
    assert_eq!(readable.context.identity.chat_id, healthy);

    // Tolerance is per record, not a blanket skip: the unreadable record still
    // fails when it is the Chat being read.
    let unreadable = StableId::parse("chat.unreadable".to_owned()).unwrap();
    assert!(history.frozen_context(&unreadable).is_err());
}
