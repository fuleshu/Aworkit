//! Chat persistence, frozen vision eligibility, and follow-up image regression.
use super::*;
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

#[test]
fn image_chat_reopens_follows_up_and_forks_without_losing_images() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut desktop = runtime(&root, provider.clone());
    configure(&mut desktop);
    let image = desktop
        .image_store()
        .import("red.png".into(), PNG.into())
        .unwrap();
    let mut first = send("image.first", 0, "");
    first.payload["attachments"] = json!([image.clone(), image.clone()]);
    let rejection = desktop.command(first.clone()).unwrap_err();
    assert!(rejection.contains("vision"), "{rejection}");
    assert!(desktop.history.conversation().unwrap().is_empty());
    let mut settings = desktop.settings_v2_snapshot();
    for provider in &mut settings.settings.providers {
        for model in &mut provider.models {
            model.capabilities.push("vision".into());
        }
    }
    desktop
        .settings_v2_commit(SettingsV2CommitInput {
            command_id: "vision.enable".into(),
            expected_version: settings.version,
            settings: settings.settings,
        })
        .unwrap();
    // Rejected commands are idempotent, so an edited intent uses a new identity.
    first.command_id = "image.accepted".into();
    desktop.command(first).unwrap();
    assert_eq!(
        desktop.history.conversation().unwrap()[0].images,
        vec![image.clone(), image.clone()]
    );
    assert_eq!(
        provider.execution_requests.lock().unwrap()[0].messages[0]
            .images
            .len(),
        2
    );
    let selected = desktop.snapshot(0).unwrap().chat.chat_id;
    drop(desktop);

    let mut desktop = runtime(&root, provider.clone());
    assert_eq!(
        desktop.image_store().preview(&image).unwrap(),
        format!("data:image/png;base64,{PNG}")
    );
    let snapshot = desktop.snapshot(0).unwrap();
    let mut follow = send("image.follow", snapshot.version, "Compare with this one");
    follow.payload["attachments"] = json!([image.clone()]);
    desktop.command(follow).unwrap();
    let requests = provider.execution_requests.lock().unwrap();
    let messages = &requests.last().unwrap().messages;
    assert_eq!(messages[0].images.len(), 2);
    assert_eq!(messages.last().unwrap().images, vec![image.clone()]);
    drop(requests);
    let snapshot = desktop.snapshot(0).unwrap();
    desktop
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "image.fork".into(),
            expected_version: snapshot.version,
            action: "fork".into(),
            target_id: Some(selected),
            payload: json!({}),
        })
        .unwrap();
    let conversation = desktop.history.conversation().unwrap();
    assert_eq!(conversation[0].images.len(), 2);
    assert_eq!(conversation[2].images, vec![image]);
}

#[test]
fn import_rejects_spoofed_corrupt_and_oversized_images() {
    let root = TempDir::new().unwrap();
    let store = super::super::super::images::ChatImageStore::new(root.path());
    assert!(store.import("fake.png".into(), "aGVsbG8=".into()).is_err());
    assert!(store.import("bad.png".into(), PNG[..30].into()).is_err());
    assert!(
        store
            .import("large.png".into(), "A".repeat(7 * 1024 * 1024))
            .is_err()
    );
}
