//! Exercise the real broker, durable tool outcomes and owner-scoped retrieval.
use super::*;
use aworkit_capability_host::context_compression::Policy;

fn fixture() -> Fixture {
    let mut f = Fixture::new();
    f.authority.context.model_context = json!({"policy":{"compression":Policy::default()}});
    let native = crate::runtime::tool_registry::native_tool("tool.context").unwrap();
    let binding = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
        capability_id: "tool.context".into(),
        configuration: json!(native.configuration),
        options: Default::default(),
        credential_bindings: vec![],
        definition: None,
    }])
    .unwrap()
    .remove(0);
    let descriptor = file_tool_descriptors()
        .unwrap()
        .remove("tool.context")
        .unwrap();
    f.authority
        .context
        .manifest
        .capability_bindings
        .push(file_tool_capability_binding(&binding, &descriptor).unwrap());
    f.authority.context.bindings.push(binding);
    f.agent.tool_ids.push("tool.context".into());
    f.write(
        "src/file.txt",
        &(0..300)
            .map(|i| {
                format!(
                    "INFO repeated worker processed observation {i} at iteration {}\n",
                    i * 17
                )
            })
            .collect::<String>(),
    );
    f
}
fn scope(f: &Fixture, id: &str) {
    f.authority
        .register_compression_scope(&f.agent, &stable(id).unwrap(), &f.request())
        .unwrap();
}
fn invoke(
    f: &Fixture,
    outer: &str,
    id: &str,
    capability: &str,
    arguments: Value,
) -> SettledModelToolCallV1 {
    f.authority
        .invoke(
            &stable(outer).unwrap(),
            1,
            &ModelToolCallV1 {
                call_id: id.into(),
                provider_call_id: Some(id.into()),
                capability_id: capability.into(),
                name: if capability == "tool.context" {
                    "aworkit_context"
                } else {
                    FILE_READ_PROVIDER_NAME
                }
                .into(),
                arguments,
                provider_context: None,
            },
            &CancellationToken::default(),
        )
        .unwrap()
}
fn read(f: &Fixture, outer: &str, id: &str) -> SettledModelToolCallV1 {
    invoke(
        f,
        outer,
        id,
        FILE_READ_CAPABILITY_ID,
        json!({"path":"src/file.txt"}),
    )
}

#[test]
fn compressed_projection_is_durable_before_use_and_replay_is_identical() {
    let mut f = fixture();
    scope(&f, "outer.compress");
    let result = read(&f, "outer.compress", "read.1");
    let events = f.committer.committed_events().unwrap();
    let archive = events
        .iter()
        .find(|e| e.kind == "context.compression")
        .unwrap();
    assert_eq!(archive.payload["projection"], result.result.content);
    assert!(
        archive.payload["original"]["content"]
            .as_str()
            .unwrap()
            .contains("observation 299")
    );
    assert!(
        archive.payload["metrics"]["afterBytes"].as_u64().unwrap()
            < archive.payload["metrics"]["beforeBytes"].as_u64().unwrap()
    );
    f.write("src/file.txt", "file changed after original read");
    f.authority.runtime.records =
        Arc::new(ToolRecordStore::open(&f.root.path().join("events.sqlite3")).unwrap());
    let replay = read(&f, "outer.compress", "read.1");
    assert_eq!(result.result, replay.result);
    assert_eq!(
        f.committer
            .committed_events()
            .unwrap()
            .iter()
            .filter(|e| e.kind == "context.compression")
            .count(),
        1
    );
}

#[test]
fn output_limit_preview_archives_uncompressible_original_and_recovers_omitted_tail() {
    let mut f = fixture();
    f.authority.context.maximum_tool_output_bytes = 1024;
    let source = format!("{}TAIL_RECEIPT_739", "ordinary prose without log rows ".repeat(300));
    f.write("src/file.txt", &source);
    scope(&f, "outer.preview");
    let result = read(&f, "outer.preview", "read.preview");
    assert_eq!(result.result.content["aworkitOutput"]["truncated"], true);
    assert!(result.result.content.to_string().len() <= 1024);
    assert!(!result.result.content.to_string().contains("TAIL_RECEIPT_739"));
    let reference = result.result.content["aworkitContext"]["reference"].clone();
    assert!(reference.is_string());
    let events = f.committer.committed_events().unwrap();
    let archive = events.iter().find(|e| e.kind == "context.compression").unwrap();
    assert_eq!(archive.payload["original"]["content"], source);
    assert_eq!(archive.payload["metrics"]["strategies"], json!(["output-limit-preview"]));
    assert_eq!(archive.payload["metrics"]["lossy"], true);
    f.write("src/file.txt", "changed file must not be read again");
    f.authority.runtime.records = Arc::new(ToolRecordStore::open(&f.root.path().join("events.sqlite3")).unwrap());
    assert_eq!(read(&f, "outer.preview", "read.preview").result, result.result);
    scope(&f, "outer.preview-recovery");
    let recovered = invoke(&f, "outer.preview-recovery", "recover.preview", "tool.context",
        json!({"operation":"read","reference":reference,"pointer":"/content","offset":source.len()-16,"limit":256}));
    assert!(!recovered.result.is_error, "{:?}", recovered.result);
    assert!(recovered.result.content.to_string().contains("TAIL_RECEIPT_739"));
}

#[test]
fn retrieval_survives_new_outer_and_rejects_other_nodes_children_and_chats() {
    let mut f = fixture();
    scope(&f, "outer.first");
    let result = read(&f, "outer.first", "read.1");
    let reference = result.result.content["aworkitContext"]["reference"].clone();
    assert!(reference.is_string());
    scope(&f, "outer.second");
    let args = json!({"operation":"search","reference":reference,"pointer":"/content","query":"observation 299"});
    let recovered = invoke(
        &f,
        "outer.second",
        "retrieve.1",
        "tool.context",
        args.clone(),
    );
    assert!(!recovered.result.is_error, "{:?}", recovered.result);
    assert!(
        recovered.result.content["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["text"].as_str().unwrap().contains("observation 299"))
    );
    let discovered = invoke(
        &f,
        "outer.second",
        "discover.1",
        "tool.context",
        json!({"operation":"search","pointer":"/content","query":"observation 299"}),
    );
    assert!(!discovered.result.is_error, "{:?}", discovered.result);
    assert_eq!(
        discovered.result.content["matches"][0]["reference"],
        reference
    );
    let original_node = f.agent.node_id.clone();
    f.agent.node_id = "another.node".into();
    scope(&f, "outer.node");
    assert!(
        invoke(&f, "outer.node", "retrieve.2", "tool.context", args.clone())
            .result
            .is_error
    );
    f.agent.node_id = original_node;
    f.agent.child = Some("child.one".into());
    scope(&f, "outer.child");
    assert!(
        invoke(
            &f,
            "outer.child",
            "retrieve.3",
            "tool.context",
            args.clone()
        )
        .result
        .is_error
    );
    f.agent.child = None;
    f.authority.context.chat_id = "other.chat".into();
    scope(&f, "outer.chat");
    assert!(
        invoke(&f, "outer.chat", "retrieve.4", "tool.context", args)
            .result
            .is_error
    );
}

#[test]
fn legacy_disabled_errors_and_unselected_retrieval_do_not_create_lossy_projections() {
    let mut f = fixture();
    f.authority.context.model_context = json!({});
    scope(&f, "outer.legacy");
    assert!(
        read(&f, "outer.legacy", "read.1")
            .result
            .content
            .get("aworkitContext")
            .is_none()
    );
    f.authority.context.model_context = json!({"policy":{"compression":{"mode":"adaptive"}}});
    let mut request = f.request();
    request.tools.retain(|t| t.capability_id != "tool.context");
    f.authority
        .register_compression_scope(&f.agent, &stable("outer.noretrieve").unwrap(), &request)
        .unwrap();
    read(&f, "outer.noretrieve", "read.2");
    for event in f
        .committer
        .committed_events()
        .unwrap()
        .iter()
        .filter(|e| e.kind == "context.compression")
    {
        assert_eq!(event.payload["metrics"]["lossy"], false);
        assert_eq!(event.payload["retrievable"], false);
    }
    let failed = invoke(
        &f,
        "outer.noretrieve",
        "missing.1",
        FILE_READ_CAPABILITY_ID,
        json!({"path":"missing.txt"}),
    );
    assert!(failed.result.is_error);
    assert!(failed.result.content.get("aworkitContext").is_none());
}

#[test]
fn retrieval_feedback_backs_off_without_rewriting_prior_projection() {
    let mut f = fixture();
    f.authority.context.model_context["policy"]["compression"]["mode"] = json!("adaptive");
    scope(&f, "outer.feedback");
    for i in 0..3 {
        let output = read(&f, "outer.feedback", &format!("read.{i}"));
        let reference = output.result.content["aworkitContext"]["reference"].clone();
        let result = invoke(
            &f,
            "outer.feedback",
            &format!("get.{i}"),
            "tool.context",
            json!({"operation":"read","reference":reference,"pointer":"/content"}),
        );
        assert!(!result.result.is_error);
    }
    read(&f, "outer.feedback", "read.4");
    let events = f.committer.committed_events().unwrap();
    let last = events
        .iter()
        .rev()
        .find(|e| e.kind == "context.compression")
        .unwrap();
    assert_eq!(last.payload["backoff"], true);
    assert_eq!(last.payload["metrics"]["lossy"], false);
    let stats = invoke(
        &f,
        "outer.feedback",
        "stats.1",
        "tool.context",
        json!({"operation":"stats"}),
    );
    assert_eq!(stats.result.content["retrievalCalls"], 3);
    assert!(stats.result.content.to_string().len() <= 256);
}

#[test]
fn child_extraction_uses_the_child_request_question() {
    let mut f = fixture();
    f.authority.context.model_context["policy"]["compression"]["mode"] = json!("adaptive");
    f.authority.context.review_messages[0].content = "parent_task".into();
    f.agent.child = Some("child.reader".into());
    let source = format!(
        "fn parent_task() {{ println!(\"PARENT EVIDENCE\"); {} }}\nfn child_task() {{ println!(\"CHILD EVIDENCE\"); {} }}",
        "let x = 1;\n".repeat(300),
        "let y = 2;\n".repeat(40)
    );
    f.write("src/child.rs", &source);
    let mut request = f.request();
    request.input = json!({"messages":[{"role":"user","content":"child_task"}]});
    f.authority
        .register_compression_scope(&f.agent, &stable("outer.child-query").unwrap(), &request)
        .unwrap();
    let result = invoke(
        &f,
        "outer.child-query",
        "read.child",
        FILE_READ_CAPABILITY_ID,
        json!({"path":"src/child.rs"}),
    );
    let projection = result.result.content.to_string();
    assert!(projection.contains("CHILD EVIDENCE"));
    assert!(!projection.contains("PARENT EVIDENCE"));
}

struct RejectArchive {
    inner: Arc<dyn SemanticEventCommitter>,
    reject: std::sync::atomic::AtomicBool,
}
impl SemanticEventCommitter for RejectArchive {
    fn commit(
        &self,
        events: Vec<SemanticEventDraft>,
    ) -> Result<Vec<crate::runtime::semantic_events::CoreEventEnvelope>, String> {
        if self.reject.load(std::sync::atomic::Ordering::SeqCst)
            && events.iter().any(|e| e.kind == "context.compression")
        {
            return Err("injected archive commit failure".into());
        }
        self.inner.commit(events)
    }
    fn committed_events(
        &self,
    ) -> Result<Vec<crate::runtime::semantic_events::CoreEventEnvelope>, String> {
        self.inner.committed_events()
    }
}

#[test]
fn failed_archive_commit_never_publishes_a_reference_and_retry_uses_settled_original() {
    let mut f = fixture();
    let committer = Arc::new(RejectArchive {
        inner: f.committer.clone(),
        reject: std::sync::atomic::AtomicBool::new(true),
    });
    f.authority.run_events = Arc::new(RunEventStream::new(
        f.authority.context.request_id.to_string(),
        f.authority.context.run_id.to_string(),
        committer.clone(),
        CancellationToken::default(),
    ));
    scope(&f, "outer.commit-failure");
    let call = ModelToolCallV1 {
        call_id: "read.retry".into(),
        provider_call_id: Some("read.retry".into()),
        capability_id: FILE_READ_CAPABILITY_ID.into(),
        name: FILE_READ_PROVIDER_NAME.into(),
        arguments: json!({"path":"src/file.txt"}),
        provider_context: None,
    };
    let result = f.authority.invoke(
        &stable("outer.commit-failure").unwrap(),
        1,
        &call,
        &CancellationToken::default(),
    );
    assert!(result.is_err());
    assert!(
        !f.committer
            .committed_events()
            .unwrap()
            .iter()
            .any(|e| e.kind == "context.compression")
    );
    f.write(
        "src/file.txt",
        "new content must not replace settled evidence",
    );
    committer
        .reject
        .store(false, std::sync::atomic::Ordering::SeqCst);
    // A failed durable stream remains poisoned for its run. Recovery constructs
    // a fresh stream and reopens broker records, just as desktop recovery does.
    f.authority.run_events = Arc::new(RunEventStream::new(
        f.authority.context.request_id.to_string(),
        f.authority.context.run_id.to_string(),
        committer,
        CancellationToken::default(),
    ));
    f.authority.runtime.records =
        Arc::new(ToolRecordStore::open(&f.root.path().join("events.sqlite3")).unwrap());
    let result = read(&f, "outer.commit-failure", "read.retry");
    assert!(result.result.content.get("aworkitContext").is_some());
    let events = f.committer.committed_events().unwrap();
    let archive = events
        .iter()
        .find(|e| e.kind == "context.compression")
        .unwrap();
    assert!(
        archive.payload["original"]["content"]
            .as_str()
            .unwrap()
            .contains("observation 299")
    );
}
