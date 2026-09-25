//! Exercise location policy through the real durable broker and capability host.
use super::*;

#[test]
fn one_filesystem_read_grant_covers_all_read_tools_and_other_chats_but_not_writes() {
    use crate::runtime::approvals::{
        ApprovalChoice, ApprovalResolution, FilesystemLocation, FilesystemSelection,
    };
    let mut f = Fixture::with_tools(&[
        FILE_LIST_CAPABILITY_ID,
        FILE_READ_CAPABILITY_ID,
        FILE_SEARCH_CAPABILITY_ID,
        FILE_GREP_CAPABILITY_ID,
        FILE_WRITE_CAPABILITY_ID,
    ]);
    let directory = f.root.path().join("reference");
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("file.txt");
    std::fs::write(&path, "reference alpha").unwrap();
    let first = call(
        &f,
        FILE_LIST_CAPABILITY_ID,
        json!({"path":directory,"pattern":"**/*"}),
    );
    let outer = stable("outer.permission-list").unwrap();
    let challenge = pending(&f, &outer, &first);
    let request = challenge.filesystem.clone().unwrap();
    let resolution = ApprovalResolution {
        choice: ApprovalChoice::AlwaysApproveInProject,
        reason: None,
        filesystem: Some(FilesystemSelection {
            access: request.access,
            location: FilesystemLocation::Directory,
            directory: Some(request.directory),
        }),
    };
    let grant = f
        .authority
        .runtime
        .filesystem_grant(
            &f.authority.context.approvals,
            &challenge.decision_id,
            &resolution,
        )
        .unwrap()
        .unwrap();
    f.authority
        .runtime
        .approvals
        .resolve_with_filesystem(&challenge.decision_id, &resolution, None, Some(&grant))
        .unwrap();
    let first_result = approve(&f, &outer, &first, challenge, true).result;
    assert!(!first_result.is_error, "{first_result:?}");
    f.authority.context.approvals.chat_id = "chat.same-project".into();
    for (index, (id, args)) in [
        (FILE_READ_CAPABILITY_ID, json!({"path":path})),
        (
            FILE_SEARCH_CAPABILITY_ID,
            json!({"path":path,"query":"alpha"}),
        ),
        (
            FILE_GREP_CAPABILITY_ID,
            json!({"path":directory,"pattern":"alpha"}),
        ),
        (
            FILE_LIST_CAPABILITY_ID,
            json!({"path":directory,"pattern":"**/*"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let result = f
            .authority
            .invoke_v1(
                &stable(&format!("outer.shared-{index}")).unwrap(),
                1,
                &call(&f, id, args),
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(!result.result.is_error, "{:?}", result.result);
    }
    let write = call(
        &f,
        FILE_WRITE_CAPABILITY_ID,
        json!({"path":path,"content":"changed"}),
    );
    pending(&f, &stable("outer.write-needs-permission").unwrap(), &write);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "reference alpha");
    f.authority.context.approvals.project_key = Some("project.other".into());
    pending(
        &f,
        &stable("outer.other-project").unwrap(),
        &call(&f, FILE_READ_CAPABILITY_ID, json!({"path":path})),
    );
    assert!(
        f.committer
            .committed_events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "approval.permission_used")
    );
}
use crate::runtime::approvals::ApprovalMode;

fn call(f: &Fixture, id: &str, args: Value) -> ModelToolCallV1 {
    let binding = f
        .authority
        .context
        .bindings
        .iter()
        .find(|b| b.capability_id == id)
        .unwrap();
    ModelToolCallV1 {
        call_id: "file.1".into(),
        provider_call_id: Some("file.1".into()),
        capability_id: id.into(),
        name: binding.provider_name.clone(),
        arguments: args,
        provider_context: None,
    }
}

fn approve(
    f: &Fixture,
    outer: &StableId,
    call: &ModelToolCallV1,
    challenge: ToolApprovalChallengeV1,
    approved: bool,
) -> SettledModelToolCallV1 {
    f.authority
        .resolve_invoke_v1_inner(
            outer,
            1,
            call,
            &ApprovalResponseV1 {
                invocation_id: stable(&challenge.invocation_id).unwrap(),
                nonce: stable(&challenge.nonce).unwrap(),
                approved,
                now_epoch_millis: current_epoch_millis(),
            },
            &CancellationToken::default(),
        )
        .unwrap()
}

fn pending(f: &Fixture, outer: &StableId, call: &ModelToolCallV1) -> ToolApprovalChallengeV1 {
    match f
        .authority
        .invoke_v1(outer, 1, call, &CancellationToken::default())
    {
        Err(WorkflowPipelineError::ToolApproval(challenge)) => challenge,
        other => panic!("expected approval, got {other:?}"),
    }
}

#[test]
fn all_file_operations_use_absolute_paths_and_approve_only_external_access() {
    for external in [false, true] {
        for id in [
            FILE_READ_CAPABILITY_ID,
            FILE_SEARCH_CAPABILITY_ID,
            FILE_LIST_CAPABILITY_ID,
            FILE_GREP_CAPABILITY_ID,
            FILE_EDIT_CAPABILITY_ID,
            FILE_WRITE_CAPABILITY_ID,
        ] {
            let f = Fixture::with_tools(&[id]);
            let directory = if external {
                f.root.path().join("external")
            } else {
                f.authority.context.workspace.root.join("files")
            };
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join("test.txt");
            std::fs::write(&path, "alpha").unwrap();
            let args = match id {
                FILE_READ_CAPABILITY_ID => json!({"path":path}),
                FILE_SEARCH_CAPABILITY_ID => json!({"path":path,"query":"alpha"}),
                FILE_LIST_CAPABILITY_ID => json!({"path":directory,"pattern":"*.txt"}),
                FILE_GREP_CAPABILITY_ID => json!({"path":directory,"pattern":"alpha"}),
                FILE_EDIT_CAPABILITY_ID => {
                    json!({"path":path,"old_string":"alpha","new_string":"beta"})
                }
                _ => json!({"path":path,"content":"beta"}),
            };
            let call = call(&f, id, args);
            let outer = stable("outer.files").unwrap();
            let result = if external {
                let challenge = pending(&f, &outer, &call);
                assert!(
                    challenge.project_scope.is_some(),
                    "external access offers an explicit filesystem project permission"
                );
                assert!(challenge.summary.contains("outside the Chat workspace"));
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    "alpha",
                    "no write before approval"
                );
                approve(&f, &outer, &call, challenge, true)
            } else {
                f.authority
                    .invoke_v1(&outer, 1, &call, &CancellationToken::default())
                    .unwrap()
            };
            assert!(!result.result.is_error, "{id}: {:?}", result.result);
            if matches!(id, FILE_EDIT_CAPABILITY_ID | FILE_WRITE_CAPABILITY_ID) {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "beta");
            } else if id == FILE_READ_CAPABILITY_ID {
                assert_eq!(result.result.content["content"], "alpha");
            } else if id == FILE_SEARCH_CAPABILITY_ID {
                assert_eq!(result.result.content["offsets"], json!([0]));
            } else {
                let key = if id == FILE_LIST_CAPABILITY_ID {
                    "entries"
                } else {
                    "matches"
                };
                assert_eq!(result.result.content[key].as_array().unwrap().len(), 1);
                assert!(
                    Path::new(result.result.content[key][0]["path"].as_str().unwrap())
                        .is_absolute()
                );
            }
        }
    }
}

#[test]
fn relative_write_and_new_file_inside_workspace_need_no_review_even_with_override() {
    let mut f = Fixture::with_tools(&[FILE_WRITE_CAPABILITY_ID]);
    f.authority.context.bindings[0].options.approval_mode = Some(ApprovalMode::AskForApproval);
    let call = call(
        &f,
        FILE_WRITE_CAPABILITY_ID,
        json!({"path":"src/new.txt","content":"new"}),
    );
    let result = f
        .authority
        .invoke_v1(
            &stable("outer.new-file").unwrap(),
            1,
            &call,
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(!result.result.is_error, "{:?}", result.result);
    assert_eq!(
        std::fs::read_to_string(f.authority.context.workspace.root.join("src/new.txt")).unwrap(),
        "new"
    );
}

#[test]
fn a_write_creates_the_directories_it_names_before_the_tool_ever_runs() {
    // The fixture creates `src` for other tests, which is why a write into a new
    // directory was never exercised: path resolution canonicalised the parent and
    // failed with a bare ENOENT before `write_v1`, whose parent creation was the
    // part under test. A path with no existing ancestor below the workspace root
    // is the production case - the first write of a scaffolded tree.
    let f = Fixture::with_tools(&[FILE_WRITE_CAPABILITY_ID]);
    let call = call(
        &f,
        FILE_WRITE_CAPABILITY_ID,
        json!({"path":"assets/sprites/hero.json","content":"{}"}),
    );
    let result = f
        .authority
        .invoke_v1(
            &stable("outer.nested-write").unwrap(),
            1,
            &call,
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(!result.result.is_error, "{:?}", result.result);
    assert_eq!(
        std::fs::read_to_string(
            f.authority
                .context
                .workspace
                .root
                .join("assets/sprites/hero.json")
        )
        .unwrap(),
        "{}"
    );
}

#[test]
fn external_denial_has_no_effect_and_full_access_uses_the_same_target() {
    for mode in [ApprovalMode::AskForApproval, ApprovalMode::FullAccess] {
        let mut f = Fixture::with_tools(&[FILE_WRITE_CAPABILITY_ID]);
        f.authority.context.approvals.mode = mode;
        let target = f.root.path().join("outside.txt");
        let call = call(
            &f,
            FILE_WRITE_CAPABILITY_ID,
            json!({"path":"../outside.txt","content":"external"}),
        );
        let outer = stable("outer.external").unwrap();
        let result = if mode == ApprovalMode::AskForApproval {
            approve(&f, &outer, &call, pending(&f, &outer, &call), false)
        } else {
            f.authority
                .invoke_v1(&outer, 1, &call, &CancellationToken::default())
                .unwrap()
        };
        assert_eq!(target.exists(), mode == ApprovalMode::FullAccess);
        assert_eq!(result.result.is_error, mode == ApprovalMode::AskForApproval);
    }
}

#[test]
fn approval_survives_store_reopen_and_rejects_replaced_target_directory() {
    let mut f = Fixture::with_tools(&[FILE_WRITE_CAPABILITY_ID]);
    let directory = f.root.path().join("external");
    std::fs::create_dir(&directory).unwrap();
    let call = call(
        &f,
        FILE_WRITE_CAPABILITY_ID,
        json!({"path":directory.join("new.txt"),"content":"must not be written"}),
    );
    let outer = stable("outer.replaced-directory").unwrap();
    let challenge = pending(&f, &outer, &call);
    f.authority.runtime.records =
        Arc::new(ToolRecordStore::open(&f.root.path().join("events.sqlite3")).unwrap());
    std::fs::rename(&directory, f.root.path().join("original")).unwrap();
    std::fs::create_dir(&directory).unwrap();
    let result = approve(&f, &outer, &call, challenge, true);
    assert!(result.result.is_error);
    assert!(!directory.join("new.txt").exists());
}

#[test]
fn legacy_frozen_binding_stays_relative_and_keeps_its_name() {
    let mut f = Fixture::with_tools(&[FILE_READ_CAPABILITY_ID]);
    let binding = &mut f.authority.context.bindings[0];
    binding.file_access_version = None;
    binding.provider_name = "aworkit_read_project_file".into();
    binding.requires_approval = false;
    f.authority.context.manifest.capability_bindings[0].approval = ApprovalRequirement::Never;
    let call = call(&f, FILE_READ_CAPABILITY_ID, json!({"path":"src/file.txt"}));
    assert_eq!(call.name, "aworkit_read_project_file");
    let result = f
        .authority
        .invoke_v1(
            &stable("outer.legacy").unwrap(),
            1,
            &call,
            &CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(result.result.content["content"], "file");
    let mut absolute = call.clone();
    absolute.arguments = json!({"path":f.authority.context.workspace.root.join("src/file.txt")});
    // An absolute path on a legacy (relative-only) binding is rejected before
    // execution and settles as a recoverable tool error, not a fatal pass error.
    let denied = f
        .authority
        .invoke_v1(
            &stable("outer.legacy-absolute").unwrap(),
            1,
            &absolute,
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(denied.result.is_error);
}

/// A regex search must accept one file the way `read` does. The model targets
/// the file it just read, and the search then covers exactly that file instead
/// of failing on its path or silently widening to its whole directory.
#[test]
fn grep_accepts_a_single_file_target_and_scans_only_that_file() {
    for external in [false, true] {
        let f = Fixture::with_tools(&[FILE_GREP_CAPABILITY_ID]);
        let directory = if external {
            f.root.path().join("external")
        } else {
            f.authority.context.workspace.root.join("files")
        };
        std::fs::create_dir(&directory).unwrap();
        let target = directory.join("provider.ts");
        std::fs::write(&target, "const maxBytes = 1;\n").unwrap();
        std::fs::write(directory.join("sibling.ts"), "const maxBytes = 2;\n").unwrap();
        let call = call(
            &f,
            FILE_GREP_CAPABILITY_ID,
            json!({"path":target,"pattern":"maxBytes"}),
        );
        let outer = stable("outer.grep-file").unwrap();
        let result = if external {
            let challenge = pending(&f, &outer, &call);
            assert!(challenge.summary.contains("outside the Chat workspace"));
            approve(&f, &outer, &call, challenge, true)
        } else {
            f.authority
                .invoke_v1(&outer, 1, &call, &CancellationToken::default())
                .unwrap()
        };
        assert!(!result.result.is_error, "{:?}", result.result);
        let matches = result.result.content["matches"].as_array().unwrap();
        assert_eq!(
            matches.len(),
            1,
            "only the named file is searched: {:?}",
            result.result.content
        );
        assert!(
            matches[0]["path"]
                .as_str()
                .unwrap()
                .ends_with("provider.ts"),
            "{:?}",
            matches[0]
        );
        assert!(
            Path::new(matches[0]["path"].as_str().unwrap()).is_absolute(),
            "the model sees a usable absolute path: {:?}",
            matches[0]
        );
    }
}

/// A listing cannot be rooted at a file, and saying so must be actionable
/// rather than a workspace error about a workspace that was never involved.
#[test]
fn list_rejects_a_file_target_with_an_actionable_error() {
    let f = Fixture::with_tools(&[FILE_LIST_CAPABILITY_ID]);
    let directory = f.authority.context.workspace.root.join("files");
    std::fs::create_dir(&directory).unwrap();
    let target = directory.join("notes.txt");
    std::fs::write(&target, "alpha").unwrap();
    let call = call(
        &f,
        FILE_LIST_CAPABILITY_ID,
        json!({"path":target,"pattern":"*.txt"}),
    );
    let result = f
        .authority
        .invoke_v1(
            &stable("outer.list-file").unwrap(),
            1,
            &call,
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(result.result.is_error);
    let message = result.result.content["error"].as_str().unwrap();
    assert!(
        message.contains("directory") && !message.contains("workspace is unavailable"),
        "{message}"
    );
}

#[cfg(windows)]
#[test]
fn junction_inside_workspace_is_classified_as_external() {
    let f = Fixture::with_tools(&[FILE_WRITE_CAPABILITY_ID]);
    let directory = f.root.path().join("external");
    std::fs::create_dir(&directory).unwrap();
    let link = f.authority.context.workspace.root.join("linked");
    let result = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let call = call(
        &f,
        FILE_WRITE_CAPABILITY_ID,
        json!({"path":"linked/new.txt","content":"external"}),
    );
    let outer = stable("outer.junction").unwrap();
    let challenge = pending(&f, &outer, &call);
    assert!(!directory.join("new.txt").exists());
    let result = approve(&f, &outer, &call, challenge, true);
    assert!(!result.result.is_error, "{:?}", result.result);
    assert_eq!(
        std::fs::read_to_string(directory.join("new.txt")).unwrap(),
        "external"
    );
}

#[test]
fn saved_file_catalog_migrates_once_without_enabling_tools_or_changing_options() {
    let mut settings = crate::runtime::settings_v2::SettingsConfigurationV2::default();
    let tool = settings
        .tools
        .iter_mut()
        .find(|t| t.id == FILE_READ_CAPABILITY_ID)
        .unwrap();
    tool.requires_project = true;
    tool.name = "Project file read".into();
    tool.options.approval_mode = Some(ApprovalMode::ApproveForMe);
    let before = tool.clone();
    assert!(settings.normalize_legacy_file_tools());
    assert!(!settings.normalize_legacy_file_tools());
    settings.validate().unwrap();
    let tool = settings
        .tools
        .iter()
        .find(|t| t.id == FILE_READ_CAPABILITY_ID)
        .unwrap();
    assert_eq!(tool.name, "File read");
    assert!(!tool.requires_project);
    assert_eq!(tool.enabled, before.enabled);
    assert_eq!(tool.options, before.options);
    assert_eq!(tool.configuration, before.configuration);
    assert!(
        before.requires_project,
        "existing frozen copies retain the prior contract"
    );
}

/// Offset and limit page one file: the selected range is returned with metadata
/// that lets the model continue, while a read without them stays whole-file.
#[test]
fn read_pages_a_file_with_offset_and_limit() {
    let f = Fixture::new();
    let body: String = (1..=200).map(|line| format!("line {line}\n")).collect();
    std::fs::write(
        f.authority.context.workspace.root.join("src/long.txt"),
        &body,
    )
    .unwrap();

    let whole = f
        .authority
        .invoke_v1(
            &stable("outer.read-whole").unwrap(),
            1,
            &call(&f, FILE_READ_CAPABILITY_ID, json!({"path":"src/long.txt"})),
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(!whole.result.is_error, "{:?}", whole.result);
    assert_eq!(whole.result.content["content"].as_str(), Some(body.as_str()));

    let page = f
        .authority
        .invoke_v1(
            &stable("outer.read-page").unwrap(),
            1,
            &call(
                &f,
                FILE_READ_CAPABILITY_ID,
                json!({"path":"src/long.txt","offset":150,"limit":2}),
            ),
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(!page.result.is_error, "{:?}", page.result);
    assert_eq!(
        page.result.content["content"].as_str(),
        Some("line 150\nline 151\n")
    );
    assert_eq!(page.result.content["firstLine"].as_u64(), Some(150));
    assert_eq!(page.result.content["lines"].as_u64(), Some(2));
    assert_eq!(page.result.content["more"].as_bool(), Some(true));
    assert_eq!(page.result.content["nextOffset"].as_u64(), Some(152));
    assert_eq!(page.result.content["truncated"].as_bool(), Some(false));

    // Invalid paging parameters fail before any content is read.
    let invalid = f.authority.invoke_v1(
        &stable("outer.read-invalid").unwrap(),
        1,
        &call(
            &f,
            FILE_READ_CAPABILITY_ID,
            json!({"path":"src/long.txt","offset":0}),
        ),
        &CancellationToken::default(),
    );
    let message = match invalid {
        Ok(settled) => settled.result.content.to_string(),
        Err(error) => error.to_string(),
    };
    assert!(message.contains("offset"), "{message}");
}
