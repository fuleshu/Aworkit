//! Production authority path: project boundaries, vision gating and durable replay.
use super::*;
use aworkit_capability_host::model_images::ModelImageResolver;
use base64::{Engine, engine::general_purpose::STANDARD};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
fn read_call(path: &str) -> ModelToolCallV1 {
    ModelToolCallV1 {
        call_id: "image.1".into(),
        provider_call_id: Some("image.1".into()),
        capability_id: "tool.image.read".into(),
        name: "aworkit_read_image".into(),
        arguments: json!({"path":path}),
        provider_context: None,
    }
}

#[test]
fn image_read_survives_source_deletion_and_settled_replay() {
    let mut f = Fixture::with_tools(&["tool.image.read"]);
    f.authority.context.model_context = json!({"imageInput":true});
    let bytes = STANDARD.decode(PNG).unwrap();
    let path = f.authority.context.workspace.root.join("image.png");
    std::fs::write(&path, &bytes).unwrap();
    let outer = stable("outer.image").unwrap();
    let call = read_call("image.png");
    let first = f
        .authority
        .invoke(&outer, 1, &call, &CancellationToken::default())
        .unwrap();
    assert!(!first.result.is_error, "{:?}", first.result);
    assert_eq!(first.result.images.len(), 1);
    std::fs::remove_file(path).unwrap();
    let replay = f
        .authority
        .invoke(&outer, 1, &call, &CancellationToken::default())
        .unwrap();
    assert_eq!(replay.result, first.result);
    let reopened = crate::runtime::images::ChatImageStore::new(f.root.path());
    assert_eq!(reopened.read(&first.result.images[0]).unwrap(), bytes);
    assert!(!serde_json::to_string(&first.result).unwrap().contains(PNG));
}

#[test]
fn image_read_rejects_nonvision_corrupt_oversized_and_escaping_input() {
    for (index, path, bytes, vision) in [
        (0, "image.png", STANDARD.decode(PNG).unwrap(), false),
        (1, "image.png", b"not an image".to_vec(), true),
        (2, "image.png", vec![0; 32 * 1024 * 1024 + 1], true),
        (3, "../outside.png", STANDARD.decode(PNG).unwrap(), true),
    ] {
        let mut f = Fixture::with_tools(&["tool.image.read"]);
        f.authority.context.model_context = json!({"imageInput":vision});
        std::fs::write(f.authority.context.workspace.root.join(path), bytes).unwrap();
        let outcome = f.authority.invoke(
            &stable(&format!("outer.invalid{index}")).unwrap(),
            1,
            &read_call(path),
            &CancellationToken::default(),
        );
        let settled = outcome.expect("invalid image input must settle without aborting the Agent");
        assert!(settled.result.is_error, "{:?}", settled.result);
        assert!(settled.result.images.is_empty());
        assert!(
            !f.root
                .path()
                .join("images")
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_some())
        );
    }
}

#[test]
fn absolute_local_image_outside_workspace_is_bounded_and_replays_without_rereading() {
    let mut f = Fixture::with_tools(&["tool.image.read"]);
    f.authority.context.model_context = json!({"imageInput":true});
    f.authority.context.approvals.project_key = None;
    let source = f.root.path().join("external image ü.png");
    let mut bytes = STANDARD.decode(PNG).unwrap();
    bytes.resize(6 * 1024 * 1024, 0); // Valid PNG with a large ancillary/trailing payload.
    std::fs::write(&source, &bytes).unwrap();
    let outer = stable("outer.absolute-image").unwrap();
    let call = read_call(source.to_str().unwrap());
    let result = f
        .authority
        .invoke(&outer, 1, &call, &CancellationToken::default())
        .unwrap();
    assert!(!result.result.is_error, "{:?}", result.result);
    assert_eq!(result.result.images.len(), 1);
    assert!(result.result.images[0].byte_length < 5 * 1024 * 1024);
    assert_eq!(
        result.result.content["source"]["preparation"]["originalBytes"],
        bytes.len()
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        bytes,
        "source must remain unchanged"
    );
    std::fs::remove_file(&source).unwrap();
    let replay = f
        .authority
        .invoke(&outer, 1, &call, &CancellationToken::default())
        .unwrap();
    assert_eq!(replay.result, result.result);
}

#[test]
fn invalid_image_arguments_settle_as_recoverable_errors_and_legacy_scope_stays_frozen() {
    let mut f = Fixture::with_tools(&["tool.image.read"]);
    f.authority.context.model_context = json!({"imageInput":true});
    let outer = stable("outer.image-errors").unwrap();
    let mut call = read_call("image.png");
    call.arguments = json!({"path":42});
    let first = f
        .authority
        .invoke(&outer, 1, &call, &CancellationToken::default())
        .unwrap();
    assert!(first.result.is_error);
    assert_eq!(
        f.authority
            .invoke(&outer, 1, &call, &CancellationToken::default())
            .unwrap()
            .result,
        first.result
    );
    let external = f.root.path().join("outside.png");
    std::fs::write(&external, STANDARD.decode(PNG).unwrap()).unwrap();
    f.authority.context.bindings[0].limit = StoredFileToolLimitV1::ImageRead;
    let legacy = f
        .authority
        .invoke(
            &stable("outer.legacy-image").unwrap(),
            1,
            &read_call(external.to_str().unwrap()),
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(legacy.result.is_error);
    assert!(legacy.result.images.is_empty());
}

#[test]
fn image_settings_migrate_once_without_enabling_tools_or_mutating_frozen_copies() {
    let mut settings = crate::runtime::settings_v2::SettingsConfigurationV2::default();
    let image = settings
        .tools
        .iter_mut()
        .find(|t| t.id == "tool.image.read")
        .unwrap();
    image.configuration = BTreeMap::from([("authorityMode".into(), json!("project_files"))]);
    image.requires_project = true;
    let frozen = image.clone();
    assert!(settings.normalize_legacy_image_tool());
    assert!(!settings.normalize_legacy_image_tool());
    settings.validate().unwrap();
    assert!(
        !settings
            .tools
            .iter()
            .find(|t| t.id == "tool.image.read")
            .unwrap()
            .enabled
    );
    assert_eq!(frozen.configuration["authorityMode"], "project_files");
    let legacy = crate::runtime::tool_loop::image_tools::freeze(
        "tool.image.read",
        &json!(frozen.configuration),
    )
    .unwrap();
    assert!(matches!(legacy.3, StoredFileToolLimitV1::ImageRead));
}
