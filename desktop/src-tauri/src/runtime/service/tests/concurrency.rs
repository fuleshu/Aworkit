//! Exercises the same dispatch boundary as native IPC with overlapping effects.
use super::*;
use crate::runtime::dispatch_chat_command;
use std::sync::mpsc;

struct HeldPipeline {
    provider: Arc<FixtureProvider>,
    started: mpsc::Sender<String>,
    released: Arc<(Mutex<BTreeSet<String>>, Condvar)>,
}
impl WorkflowPipelinePort for HeldPipeline {
    fn execute(
        &self,
        request: WorkflowExecutionRequestV1,
    ) -> Result<WorkflowExecutionResultV1, String> {
        let chat = request.chat_id.to_string();
        self.started.send(chat.clone()).unwrap();
        let (lock, ready) = &*self.released;
        let (released, timeout) = ready
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(10), |released| {
                !released.contains(&chat)
            })
            .unwrap();
        if timeout.timed_out() {
            return Err("test release timed out".into());
        }
        drop(released);
        FixtureWorkflowPipeline {
            provider: self.provider.clone(),
            goal: Mutex::new(None),
        }
        .execute(request)
    }
}

fn navigation(id: &str, action: &str, target: Option<String>) -> UiCommandInput {
    UiCommandInput {
        schema_version: 1,
        command_id: id.into(),
        expected_version: 0,
        action: action.into(),
        target_id: target,
        payload: json!({}),
    }
}

#[test]
fn concurrent_chat_workers_leave_navigation_and_other_chats_available() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider.clone());
    configure(&mut core);
    let (started, receive) = mpsc::channel();
    let released = Arc::new((Mutex::new(BTreeSet::new()), Condvar::new()));
    core.pipeline = Arc::new(HeldPipeline {
        provider,
        started,
        released: released.clone(),
    });
    let a = core.snapshot(0).unwrap().chat.chat_id;
    let core = Arc::new(Mutex::new(core));
    let spawn = |id: &str, target: &str| {
        let core = core.clone();
        let mut command = send(id, 0, target);
        command.target_id = Some(target.into());
        thread::spawn(move || dispatch_chat_command(core, command))
    };
    let first = spawn("concurrent.a", &a);
    assert_eq!(receive.recv_timeout(Duration::from_secs(3)).unwrap(), a);
    dispatch_chat_command(core.clone(), navigation("new.b", "new_chat", None)).unwrap();
    let b = core.lock().unwrap().snapshot(0).unwrap().chat.chat_id;
    let second = spawn("concurrent.b", &b);
    assert_eq!(receive.recv_timeout(Duration::from_secs(3)).unwrap(), b);
    {
        let core = core.lock().unwrap();
        let live = core.snapshot_for_chat(0, Some(&a)).unwrap();
        assert_eq!(live.active_chat_ids.len(), 2);
        assert!(!live.chat.recovery_pending);
        assert_eq!(live.chat.chat_id, a);
        assert!(core.prepare_chat_worker(&a, "duplicate.a").is_err());
    }
    assert!(
        dispatch_chat_command(
            core.clone(),
            navigation("delete.b", "delete_chat", Some(b.clone()))
        )
        .unwrap_err()
        .contains("Stop this Chat")
    );
    dispatch_chat_command(core.clone(), navigation("new.c", "new_chat", None)).unwrap();
    let c = core.lock().unwrap().snapshot(0).unwrap().chat.chat_id;
    for id in [&b, &a] {
        released.0.lock().unwrap().insert(id.clone());
        released.1.notify_all();
    }
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    let core = core.lock().unwrap();
    assert_eq!(core.snapshot(0).unwrap().chat.chat_id, c);
    assert!(core.snapshot(0).unwrap().active_chat_ids.is_empty());
    for (own, other) in [(&a, &b), (&b, &a)] {
        let snapshot = core.snapshot_for_chat(0, Some(own)).unwrap();
        assert!(!snapshot.chat.recovery_pending);
        assert!(snapshot.events.iter().all(|event| &event.stream_id == own));
        let conversation = core.history.for_chat(own).unwrap().conversation().unwrap();
        assert_eq!(conversation[0].content, *own);
        assert!(!conversation.iter().any(|message| message.content == *other));
    }
}
