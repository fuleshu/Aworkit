//! Native process-tree regressions, using this test executable as a controlled child.
use aworkit_capability_host::{
    CancellationToken, ProcessOutputCursor, ProcessSession, ProcessSpecV1,
};
use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
    process::Command,
    time::{Duration, Instant},
};

fn spec(mode: &str) -> ProcessSpecV1 {
    ProcessSpecV1 {
        program: std::env::current_exe().unwrap(),
        arguments: vec![
            "--ignored".into(),
            "--exact".into(),
            "process_fixture".into(),
            "--nocapture".into(),
        ],
        working_directory: None,
        environment: BTreeMap::from([("AWORKIT_SESSION_FIXTURE".into(), mode.into())]),
        timeout: Duration::from_millis(400),
        maximum_output_bytes: 4096,
        cancellation_grace: Duration::from_millis(20),
    }
}

#[test]
#[ignore = "launched as a child by the lifecycle tests"]
fn process_fixture() {
    match std::env::var("AWORKIT_SESSION_FIXTURE").unwrap().as_str() {
        "descendant" | "silent_descendant" => {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--ignored", "--exact", "process_fixture", "--nocapture"])
                .env("AWORKIT_SESSION_FIXTURE", "sleep");
            if std::env::var("AWORKIT_SESSION_FIXTURE").unwrap() == "silent_descendant" {
                command
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            command.spawn().unwrap();
        }
        "sleep" => {
            println!("ready");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(20));
        }
        "input" => {
            for line in std::io::stdin().lock().lines() {
                println!("echo:{}", line.unwrap());
                std::io::stdout().flush().unwrap();
            }
        }
        _ => panic!("unknown fixture"),
    }
}

fn terminal(session: &ProcessSession) -> aworkit_capability_host::ProcessSnapshot {
    let start = Instant::now();
    loop {
        let snapshot = session.snapshot().unwrap();
        if !snapshot.running {
            return snapshot;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "monitor did not settle: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn soft_wait_preserves_job_and_stdin_can_be_closed() {
    let directory = tempfile::tempdir().unwrap();
    let session = ProcessSession::start(&spec("input"), directory.path(), true).unwrap();
    let output = session
        .output(
            ProcessOutputCursor::default(),
            4096,
            Duration::from_millis(100),
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(output.snapshot.running);
    session.input(b"hello\n".to_vec(), true).unwrap();
    assert!(terminal(&session).tree_empty);
    let output = session
        .output(
            ProcessOutputCursor::default(),
            4096,
            Duration::ZERO,
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("echo:hello"));
    let next = session
        .output(
            output.next,
            4096,
            Duration::ZERO,
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(next.stdout.is_empty() && next.stderr.is_empty());
}

#[test]
fn descendants_stay_owned_after_root_exit_even_without_pipes() {
    for mode in ["descendant", "silent_descendant"] {
        let directory = tempfile::tempdir().unwrap();
        let session = ProcessSession::start(&spec(mode), directory.path(), false).unwrap();
        let started = Instant::now();
        while !session.snapshot().unwrap().root_exited {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            session.snapshot().unwrap().running,
            "descendant must still be tracked"
        );
        session.stop();
        let stopped = terminal(&session);
        assert!(stopped.tree_empty, "{stopped:?}");
        assert!(stopped.stopped);
    }
}

#[test]
fn legacy_timeout_cannot_hang_on_inherited_pipes_after_root_exit() {
    // This guards the regression that a root exiting while a descendant still
    // holds the inherited pipes must not stall output collection or termination.
    //
    // The clock starts after the child exists: launching re-hashes the fixture
    // executable twice, and this test binary is over a hundred megabytes, so
    // the launch cost is orders of magnitude larger than the behavior under
    // test. `ProcessRunner::run_controlled`'s own timeout and cleanup facts are
    // covered by the process-runner tests, which launch a small executable.
    let directory = tempfile::tempdir().unwrap();
    let session = ProcessSession::start(&spec("descendant"), directory.path(), false).unwrap();
    let started = Instant::now();

    // The root exits immediately; its descendant keeps running and keeps the
    // inherited stdout/stderr handles open.
    while !session.snapshot().unwrap().root_exited {
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }

    // Collecting output must return on the soft deadline, not on pipe EOF.
    let collected = Instant::now();
    let output = session
        .output(
            ProcessOutputCursor::default(),
            4096,
            Duration::from_millis(50),
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(
        collected.elapsed() < Duration::from_secs(5),
        "output collection must not wait on pipes a surviving descendant holds"
    );
    assert!(
        output.snapshot.running,
        "the descendant must still be tracked after the root exited"
    );

    // Stopping the session still terminates the survivor's process group.
    session.stop();
    let stopped = terminal(&session);
    assert!(stopped.tree_empty, "{stopped:?}");
    assert!(stopped.stopped, "{stopped:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "{stopped:?}");
}
