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
        (2, "image.png", vec![0; 5 * 1024 * 1024 + 1], true),
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
        match outcome {
            Ok(settled) => {
                assert!(settled.result.is_error, "{:?}", settled.result);
                assert!(settled.result.images.is_empty());
            }
            Err(_) => {}
        }
        assert!(
            !f.root
                .path()
                .join("images")
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_some())
        );
    }
}
