use super::*;
use aworkit_trusted_core::ProjectCoordinator;

#[test]
fn filesystem_grants_enforce_owner_access_containment_identity_and_revocation() {
    let root = tempfile::tempdir().unwrap();
    let reference = root.path().join("reference");
    std::fs::create_dir_all(reference.join("src")).unwrap();
    let other = root.path().join("reference-other");
    std::fs::create_dir(&other).unwrap();
    let projects = ProjectCoordinator::open(root.path().join("projects")).unwrap();
    let target = std::fs::canonicalize(&reference)
        .unwrap()
        .join("src/file.txt");
    let context = ApprovalContext {
        chat_id: "chat.one".into(),
        project_key: Some("project.one".into()),
        project_name: Some("One".into()),
        ..Default::default()
    };
    let selection = FilesystemSelection {
        access: FilesystemAccess::Read,
        location: FilesystemLocation::Directory,
        directory: Some(reference.to_string_lossy().into()),
    };
    let grant = FilesystemGrant::create(
        &context,
        &ApprovalChoice::AlwaysApproveInProject,
        &selection,
        FilesystemAccess::Read,
        &target,
        &projects,
    )
    .unwrap();
    let mut next = context.clone();
    next.chat_id = "chat.two".into();
    assert!(grant.permits(&next, FilesystemAccess::Read, &target, &projects));
    assert!(!grant.permits(&next, FilesystemAccess::Write, &target, &projects));
    assert!(!grant.permits(
        &next,
        FilesystemAccess::Read,
        &std::fs::canonicalize(&other).unwrap().join("file.txt"),
        &projects
    ));
    next.project_key = Some("project.other".into());
    assert!(!grant.permits(&next, FilesystemAccess::Read, &target, &projects));
    assert!(
        FilesystemGrant::create(
            &context,
            &ApprovalChoice::AlwaysApproveInProject,
            &selection,
            FilesystemAccess::Write,
            &target,
            &projects
        )
        .is_err()
    );
    assert!(
        FilesystemGrant::create(
            &context,
            &ApprovalChoice::AlwaysApproveInProject,
            &selection,
            FilesystemAccess::Read,
            &std::fs::canonicalize(&other).unwrap(),
            &projects
        )
        .is_err()
    );
    let chat = FilesystemGrant::create(
        &context,
        &ApprovalChoice::ApproveForChat,
        &selection,
        FilesystemAccess::Read,
        &target,
        &projects,
    )
    .unwrap();
    assert!(chat.permits(&context, FilesystemAccess::Read, &target, &projects));
    assert!(!chat.permits(&next, FilesystemAccess::Read, &target, &projects));
    let all = FilesystemSelection {
        access: FilesystemAccess::Write,
        location: FilesystemLocation::AllExternal,
        directory: None,
    };
    let all_grant = FilesystemGrant::create(
        &context,
        &ApprovalChoice::AlwaysApproveInProject,
        &all,
        FilesystemAccess::Write,
        &target,
        &projects,
    )
    .unwrap();
    assert!(all_grant.permits(&context, FilesystemAccess::Read, &other, &projects));
    assert!(all_grant.permits(&context, FilesystemAccess::Write, &other, &projects));
    assert!(!all_grant.permits(&next, FilesystemAccess::Read, &other, &projects));
    let database = root.path().join("permissions.sqlite3");
    let store = ApprovalStore::open(&database).unwrap();
    let resolution = ApprovalResolution {
        choice: ApprovalChoice::AlwaysApproveInProject,
        reason: None,
        filesystem: Some(selection),
    };
    store
        .resolve_with_filesystem("decision.one", &resolution, None, Some(&grant))
        .unwrap();
    drop(store);
    let store = ApprovalStore::open(&database).unwrap();
    assert_eq!(store.filesystem_grants().unwrap(), vec![grant.clone()]);
    assert_eq!(
        store.resolution("decision.one").unwrap(),
        Some(resolution.clone())
    );
    assert!(
        store
            .resolve_with_filesystem("decision.one", &ApprovalResolution::once(true), None, None)
            .is_err()
    );
    store.revoke_filesystem(&grant.id).unwrap();
    store
        .resolve_with_filesystem("decision.one", &resolution, None, Some(&grant))
        .unwrap();
    assert!(store.filesystem_grants().unwrap().is_empty());
    std::fs::rename(&reference, root.path().join("original-reference")).unwrap();
    std::fs::create_dir_all(reference.join("src")).unwrap();
    assert!(!grant.permits(&context, FilesystemAccess::Read, &target, &projects));
}

#[test]
fn filesystem_selection_cannot_be_smuggled_into_once_denial_or_legacy_commands() {
    use crate::runtime::service::approval_control::parse_approval_resolution;
    // ApprovalResolution validation also protects non-UI callers.
    let selection = FilesystemSelection {
        access: FilesystemAccess::Read,
        location: FilesystemLocation::AllExternal,
        directory: None,
    };
    for choice in [ApprovalChoice::ApproveOnce, ApprovalChoice::Deny] {
        assert!(
            ApprovalResolution {
                choice,
                reason: None,
                filesystem: Some(selection.clone())
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        parse_approval_resolution(&serde_json::json!({"approved":true,"filesystem":selection}))
            .is_err()
    );
}
