use super::*;

#[test]
fn discovers_capacity_for_drafts_and_freezes_it_without_editing_settings() {
    let root = TempDir::new().unwrap();
    let mut desktop = runtime(&root, Arc::new(FixtureProvider::new()));
    configure(&mut desktop);
    let before = desktop.snapshot(0).unwrap();
    let settings = desktop.settings_v2_snapshot();
    let model = desktop.context_model(&before.chat.chat_id, Some("workflow.simple-chat")).unwrap().unwrap();
    assert_eq!(model.context_window, Some(32_768));
    assert_eq!(desktop.settings_v2_snapshot().version, settings.version);
    assert!(desktop.context_model("chat.unrelated", Some("workflow.simple-chat")).is_err());
    desktop.command(send("capacity.start", 0, "Hello")).unwrap();
    let frozen = desktop.history.current_frozen_context().unwrap().unwrap();
    assert_eq!(frozen.context.model_snapshot.context_window, Some(32_768));
    assert_eq!(desktop.snapshot(0).unwrap().context_model.unwrap().context_window, Some(32_768));
}
