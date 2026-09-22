//! Opt-in live gate against a real installed Codex App Server.
//!
//! This makes one real model call, so it is ignored by default:
//!
//! ```text
//! cargo test -p aworkit-capability-host --test codex_one_shot_live -- --ignored
//! ```
//!
//! `AWORKIT_CODEX_EXECUTABLE` overrides the resolved `codex` binary. Native
//! Codex configuration and login stay authoritative: the delegation runs
//! unattended under the `never` permission mode with no approval, and the
//! process tree is torn down when the run settles.

use std::{env, path::PathBuf, time::Duration};

use aworkit_capability_host::{
    CancellationToken, CodexOneShotBackendV1, CodexOneShotConfigV1, CodexOneShotLimitsV1,
    CodexPermissionModeV1, ExternalAgentBackendV1, OneShotDelegationV1, SubagentStopReasonV1,
};
use aworkit_protocol::StableId;
use tempfile::TempDir;

fn codex_executable() -> Option<PathBuf> {
    if let Some(configured) = env::var_os("AWORKIT_CODEX_EXECUTABLE").map(PathBuf::from) {
        return configured.is_file().then_some(configured);
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join("codex"))
        .find(|candidate| candidate.is_file())
}

#[test]
#[ignore = "makes one real Codex model call"]
fn a_real_codex_delegation_returns_its_final_answer() {
    let Some(executable) = codex_executable() else {
        eprintln!("skipped: no codex executable was found");
        return;
    };
    let directory = TempDir::new().expect("temporary directory");
    let backend = CodexOneShotBackendV1::new(CodexOneShotConfigV1 {
        name: "codex".to_owned(),
        executable,
        arguments: vec!["app-server".to_owned()],
        working_directory: Some(directory.path().to_path_buf()),
        inherit_environment: true,
        environment: Vec::new(),
        permission_mode: CodexPermissionModeV1::Never,
        limits: CodexOneShotLimitsV1::default(),
    })
    .expect("valid configuration");

    let outcome = backend.run(
        &OneShotDelegationV1 {
            run_id: StableId::parse("run.live").expect("stable id"),
            task: "Reply with the single word: ready".to_owned(),
            working_directory: directory.path().to_path_buf(),
            deadline: Duration::from_secs(300),
            options: Default::default(),
        },
        &CancellationToken::default(),
    );

    assert_eq!(
        outcome.stop_reason,
        SubagentStopReasonV1::Completed,
        "live Codex run failed: {:?}",
        outcome.diagnostic
    );
    let answer = outcome.answer.unwrap_or_default();
    assert!(
        answer.to_lowercase().contains("ready"),
        "unexpected live answer: {answer:?}"
    );
}
