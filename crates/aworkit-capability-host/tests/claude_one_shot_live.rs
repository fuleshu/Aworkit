//! Opt-in live gate against a real installed Claude Code CLI.
//!
//! This makes one real model call, so it is ignored by default:
//!
//! ```text
//! cargo test -p aworkit-capability-host --test claude_one_shot_live -- --ignored
//! ```
//!
//! `AWORKIT_CLAUDE_EXECUTABLE` overrides the resolved `claude` binary. The test
//! accepts either branch on purpose: an authenticated CLI must return its final
//! answer, and a signed-out CLI must be reported as an actionable access
//! failure rather than a generic error. Native Claude configuration and login
//! stay authoritative, and the process tree is torn down when the run settles.

use std::{env, path::PathBuf, time::Duration};

use aworkit_capability_host::{
    CancellationToken, ClaudeOneShotBackendV1, ClaudeOneShotConfigV1, ClaudeOneShotLimitsV1,
    ClaudePermissionModeV1, ExternalAgentBackendV1, OneShotDelegationV1, SubagentStopReasonV1,
};
use aworkit_protocol::StableId;
use tempfile::TempDir;

fn claude_executable() -> Option<PathBuf> {
    if let Some(configured) = env::var_os("AWORKIT_CLAUDE_EXECUTABLE").map(PathBuf::from) {
        return configured.is_file().then_some(configured);
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join("claude"))
        .find(|candidate| candidate.is_file())
}

#[test]
#[ignore = "makes one real Claude Code model call"]
fn a_real_claude_delegation_answers_or_reports_missing_authentication() {
    let Some(executable) = claude_executable() else {
        eprintln!("skipped: no claude executable was found");
        return;
    };
    let directory = TempDir::new().expect("temporary directory");
    let backend = ClaudeOneShotBackendV1::new(ClaudeOneShotConfigV1 {
        name: "claude-code".to_owned(),
        executable,
        arguments: Vec::new(),
        working_directory: Some(directory.path().to_path_buf()),
        inherit_environment: true,
        environment: Vec::new(),
        permission_mode: ClaudePermissionModeV1::DontAsk,
        limits: ClaudeOneShotLimitsV1::default(),
    })
    .expect("valid configuration");

    let outcome = backend.run(
        &OneShotDelegationV1 {
            run_id: StableId::parse("run.live.claude").expect("stable id"),
            task: "Reply with the single word: ready".to_owned(),
            working_directory: directory.path().to_path_buf(),
            deadline: Duration::from_secs(300),
            options: Default::default(),
        },
        &CancellationToken::default(),
    );

    match outcome.stop_reason {
        SubagentStopReasonV1::Completed => {
            let answer = outcome.answer.unwrap_or_default();
            assert!(
                answer.to_lowercase().contains("ready"),
                "unexpected live answer: {answer:?}"
            );
        }
        SubagentStopReasonV1::AccessPolicy => {
            let diagnostic = outcome.diagnostic.unwrap_or_default();
            assert!(
                diagnostic.contains("not authenticated"),
                "a signed-out CLI must be reported with the fixed hint: {diagnostic:?}"
            );
        }
        other => panic!(
            "unexpected live outcome {other:?}: {:?}",
            outcome.diagnostic
        ),
    }
}
