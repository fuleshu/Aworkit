//! Native registry tests exercise ownership, recovery, final collection and Stop.
use super::{registry::JobRegistry, *};
use aworkit_capability_host::ProcessSpecV1;
use std::time::Instant;

fn spec(command: &str) -> ProcessSpecV1 {
    BuiltInProcessTools::<NativeProcessPort>::shell_spec(&ShellInvocationV1 {
        mode: ToolAuthorityModeV1::HostShell,
        shell_program: shell_program().unwrap(),
        command_text: command.into(),
        working_directory: None,
        environment: BTreeMap::new(),
        limits: HostToolLimitsV1::default(),
    })
    .unwrap()
}
fn long_command() -> &'static str {
    #[cfg(windows)]
    {
        "powershell -NoProfile -NonInteractive -Command Start-Sleep -Seconds 20"
    }
    #[cfg(not(windows))]
    {
        "sleep 20"
    }
}
fn wait_terminal(jobs: &JobRegistry, owner: &str, id: &str) -> Value {
    let start = Instant::now();
    loop {
        let out = jobs
            .output(
                owner,
                id,
                None,
                65536,
                Duration::from_millis(100),
                &CancellationToken::default(),
            )
            .unwrap();
        jobs.acknowledge(owner, &out).unwrap();
        if out["running"] == false {
            return out;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{out}");
    }
}

#[test]
fn jobs_enforce_owner_deduplicate_start_and_require_collection() {
    let root = tempfile::tempdir().unwrap();
    let jobs = JobRegistry::open(root.path().join("jobs")).unwrap();
    let id = jobs
        .start(
            "chat.a",
            "invocation.one",
            &spec(long_command()),
            false,
            CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(
        id,
        jobs.start(
            "chat.a",
            "invocation.one",
            &spec("echo forbidden-replay"),
            false,
            CancellationToken::default()
        )
        .unwrap()
    );
    assert!(
        jobs.output(
            "chat.b",
            &id,
            None,
            4096,
            Duration::ZERO,
            &CancellationToken::default()
        )
        .is_err()
    );
    for op in ["job_stop", "job_keep", "job_input"] {
        assert!(
            jobs.control(
                "chat.b",
                op,
                &json!({"jobId":id,"text":"x","reason":"x"}),
                &CancellationToken::default()
            )
            .is_err()
        );
    }
    assert!(jobs.completion_notice("chat.a").unwrap().is_some());
    assert!(jobs.completion_notice("chat.b").unwrap().is_none());
    let out = jobs
        .control(
            "chat.a",
            "job_stop",
            &json!({"jobId":id}),
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(
        jobs.completion_notice("chat.a").unwrap().is_some(),
        "uncommitted output must remain outstanding"
    );
    jobs.acknowledge("chat.a", &out).unwrap();
    assert_eq!(out["treeEmpty"], true, "{out}");
    assert!(jobs.completion_notice("chat.a").unwrap().is_none());
}

#[test]
fn explicit_retention_and_next_turn_stop_are_observed() {
    let root = tempfile::tempdir().unwrap();
    let jobs = JobRegistry::open(root.path().join("jobs")).unwrap();
    let id = jobs
        .start(
            "chat.a",
            "one",
            &spec(long_command()),
            false,
            CancellationToken::default(),
        )
        .unwrap();
    jobs.control(
        "chat.a",
        "job_keep",
        &json!({"jobId":id,"reason":"User needs the development server"}),
        &CancellationToken::default(),
    )
    .unwrap();
    assert!(jobs.completion_notice("chat.a").unwrap().is_none());
    let next_pass = CancellationToken::default();
    jobs.attach("chat.a", next_pass.clone());
    next_pass.cancel();
    let result = wait_terminal(&jobs, "chat.a", &id);
    assert_eq!(result["treeEmpty"], true, "{result}");
    assert_eq!(result["status"], "stopped");
}

#[test]
fn reopened_registry_never_replays_and_keeps_output_cursor() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("jobs");
    let jobs = JobRegistry::open(path.clone()).unwrap();
    let id = jobs
        .start(
            "chat.a",
            "completed",
            &spec("echo durable-output"),
            false,
            CancellationToken::default(),
        )
        .unwrap();
    let out = wait_terminal(&jobs, "chat.a", &id);
    assert_eq!(out["exitCode"], 0);
    let running = jobs
        .start(
            "chat.a",
            "uncertain",
            &spec(long_command()),
            false,
            CancellationToken::default(),
        )
        .unwrap();
    drop(jobs);
    std::thread::sleep(Duration::from_millis(300));
    let jobs = JobRegistry::open(path).unwrap();
    let reread = jobs
        .output(
            "chat.a",
            &id,
            None,
            4096,
            Duration::ZERO,
            &CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(reread["stdout"], "");
    assert_eq!(
        running,
        jobs.start(
            "chat.a",
            "uncertain",
            &spec("echo must-not-run"),
            false,
            CancellationToken::default()
        )
        .unwrap()
    );
    let interrupted = jobs
        .output(
            "chat.a",
            &running,
            None,
            4096,
            Duration::ZERO,
            &CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(interrupted["status"], "interrupted");
    assert_eq!(interrupted["running"], false);
}

#[test]
fn restart_recovers_output_written_after_the_last_lifecycle_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("jobs");
    let jobs = JobRegistry::open(path.clone()).unwrap();
    let id = jobs
        .start(
            "chat.a",
            "crash-output",
            &spec(long_command()),
            false,
            CancellationToken::default(),
        )
        .unwrap();
    // Emulate output flushed immediately before the host goes away. The
    // persisted start snapshot still reports zero bytes.
    std::fs::write(path.join(&id).join("stdout.log"), vec![b'x'; 100_000]).unwrap();
    drop(jobs);
    std::thread::sleep(Duration::from_millis(300));
    let jobs = JobRegistry::open(path).unwrap();
    let out = jobs
        .output(
            "chat.a",
            &id,
            None,
            4096,
            Duration::ZERO,
            &CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(out["stdout"].as_str().unwrap().len(), 4096);
    assert_eq!(out["moreOutput"], true);
    jobs.acknowledge("chat.a", &out).unwrap();
    assert!(jobs.completion_notice("chat.a").unwrap().is_some());
}
