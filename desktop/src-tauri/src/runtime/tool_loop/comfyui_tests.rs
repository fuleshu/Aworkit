//! Freeze, validation and in-process execution of one native ComfyUI workflow tool.
//!
//! The tool runs through the real broker and capability authority against a stub
//! ComfyUI HTTP server, so the path an Agent uses is covered end to end: the
//! Settings-derived descriptor that admits the call, the frozen binding, the
//! in-process dispatch and the stored image evidence.
use super::*;
use aworkit_capability_host::AdapterRegistry;
use aworkit_protocol::{AttestedExtensionSetV1, attested_extension_set_hash_v1};

const COMFYUI_TOOL: &str = "comfyui.krea";

/// One Settings-derived workflow tool binding with a required prompt and an
/// optional prefix, both bound to real inputs of the shipped stub workflow.
fn frozen_workflow_tool(endpoint: &str, workflow_path: &str) -> StoredFileToolBindingV1 {
    let mut tool = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
        options: Default::default(),
        capability_id: COMFYUI_TOOL.into(),
        configuration: json!({
            "endpoint": endpoint,
            "workflowPath": workflow_path,
            "parameters": [
                {"name":"prompt","nodeId":"6","inputName":"text","valueKind":"string","required":true,"choices":[]},
                {"name":"prefix","nodeId":"9","inputName":"filename_prefix","valueKind":"string","required":false,"choices":[]}
            ],
        }),
        credential_bindings: Vec::new(),
        definition: Some(ModelToolDefinitionV1 {
            capability_id: COMFYUI_TOOL.into(),
            name: "krea_image".into(),
            description: "Generate an image".into(),
            input_schema: json!({
                "type":"object",
                "properties":{"prompt":{"type":"string"},"prefix":{"type":"string"}},
                "required":["prompt"],
                "additionalProperties":false
            }),
        }),
    }])
    .unwrap()
    .remove(0);
    // A workflow run is a per-invocation decision in production. This test
    // resolves it as already approved instead of opening an interactive
    // challenge, exactly as the approval store does after a user says yes.
    assert!(tool.requires_approval);
    tool.requires_approval = false;
    tool
}

/// A 1x1 RGBA PNG, because stored image evidence is decoded before it is kept.
fn tiny_png() -> Vec<u8> {
    use image::ImageEncoder;
    let mut buffer = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buffer)
        .write_image(&[0_u8, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    buffer
}

/// Serves the three requests one workflow run makes: queue, history and view.
fn spawn_comfyui_stub() -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let image = tiny_png();
        let mut served = 0;
        while served < 3 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 8192];
            let read = std::io::Read::read(&mut stream, &mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let (content_type, body) = if request.starts_with("POST /prompt") {
                ("application/json", br#"{"prompt_id":"fixture-prompt"}"#.to_vec())
            } else if request.starts_with("GET /history/") {
                (
                    "application/json",
                    br#"{"fixture-prompt":{"outputs":{"9":{"images":[{"filename":"krea_00001_.png","subfolder":"","type":"output"}]}}}}"#.to_vec(),
                )
            } else {
                ("image/png", image.clone())
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let mut out = response.into_bytes();
            out.extend_from_slice(&body);
            let _ = std::io::Write::write_all(&mut stream, &out);
            served += 1;
        }
    });
    (format!("http://{address}"), handle)
}

#[test]
fn an_unknown_or_missing_comfyui_argument_is_refused_before_the_workflow_runs() {
    let tool = frozen_workflow_tool("http://127.0.0.1:1", "C:\\workflows\\krea.json");
    // The optional parameter may be omitted; the required one may not.
    validate_call_arguments(&tool, &json!({"prompt": "a quiet harbour"})).unwrap();
    validate_call_arguments(&tool, &json!({"prompt": "x", "prefix": "Krea"})).unwrap();
    assert!(validate_call_arguments(&tool, &json!({"prefix": "Krea"})).is_err());
    assert!(validate_call_arguments(&tool, &json!({"prompt": "x", "bogus": 1})).is_err());
    assert!(validate_call_arguments(&tool, &json!({"prompt": 7})).is_err());
}

#[test]
fn a_frozen_comfyui_workflow_tool_is_admitted_and_records_its_image() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let workflow_path = root.path().join("krea.json");
    std::fs::write(
        &workflow_path,
        br#"{"6":{"class_type":"CLIPTextEncode","inputs":{"text":"a quiet harbour"}},"9":{"class_type":"SaveImage","inputs":{"filename_prefix":"Krea"}}}"#,
    )
    .unwrap();
    let (endpoint, server) = spawn_comfyui_stub();

    let projects = ProjectCoordinator::open(root.path().join("projects")).unwrap();
    let descriptors = file_tool_descriptors().unwrap();
    let mut registry = AdapterRegistry::default();
    for descriptor in descriptors.values() {
        registry.register_capability(descriptor.clone()).unwrap();
    }
    let generation = ProcessGeneration(41);
    let mut attested = AttestedExtensionSetV1 {
        host_id: stable("host.comfyui-test").unwrap(),
        host_generation: generation,
        host_protocol: 1,
        extensions: Vec::new(),
        set_hash: String::new(),
    };
    attested.set_hash = attested_extension_set_hash_v1(&attested).unwrap();
    let key = Arc::new(CoreAuthenticationKey::random().unwrap());
    let host = Arc::new(
        CapabilityHost::from_attested_registry(
            registry.materialize_attested_set(&attested).unwrap(),
            key.copy(),
            2,
        )
        .unwrap(),
    );
    let database = root.path().join("invocations.sqlite3");
    let runtime = FileToolAuthorityRuntimeV1::open(
        &database,
        crate::runtime::images::ChatImageStore::new(root.path()),
        projects.clone(),
        host,
        descriptors.clone(),
        generation,
        key,
        Arc::new(aworkit_trusted_core::NativeCredentialStore::new()),
    )
    .unwrap();

    let tool = frozen_workflow_tool(&endpoint, &workflow_path.to_string_lossy());
    let limit = match &tool.limit {
        StoredFileToolLimitV1::ComfyUi {
            endpoint,
            workflow_path,
            parameters,
        } => (endpoint.clone(), workflow_path.clone(), parameters.len()),
        other => panic!("expected a frozen ComfyUI limit, got {other:?}"),
    };
    assert_eq!(limit.2, 2);
    assert!(!descriptors.contains_key(COMFYUI_TOOL), "generated, not compiled in");

    // Registry admission: a Settings-derived descriptor exists for this Run and
    // the frozen binding carries its exact version and hash into the manifest.
    let descriptor = comfyui_tool_descriptor(COMFYUI_TOOL).unwrap();
    assert_eq!(descriptor.capability_id, COMFYUI_TOOL);
    let binding = file_tool_capability_binding_with_nodes(
        &tool,
        &descriptor,
        vec!["agent".to_owned()],
    )
    .unwrap();
    assert_eq!(
        binding.adapter_id.as_str(),
        crate::runtime::comfyui::COMFYUI_ADAPTER_ID
    );
    assert_eq!(binding.descriptor_hash, descriptor.version_hash);
    assert_eq!(binding.approval, ApprovalRequirement::Never);

    let definitions = vec![ModelToolDefinitionV1 {
        capability_id: tool.capability_id.clone(),
        name: tool.provider_name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
    }];
    let authority = runtime.bind(FrozenFileToolAuthorityContextV1 {
        delegation: None,
        chat_id: "chat.comfyui".into(),
        approvals: Default::default(),
        review_messages: vec![super::super::pipeline::WorkflowMessageV1 {
            role: "user".into(),
            content: "Draw a quiet harbour".into(),
            images: Vec::new(),
        }],
        manifest: AuthorityManifestV1 {
            manifest_id: stable("manifest.comfyui-test").unwrap(),
            manifest_hash: format!("sha256:{}", "1".repeat(64)),
            capability_bindings: vec![binding],
            summary: "comfyui test".into(),
        },
        run_id: stable("run.comfyui-test").unwrap(),
        request_id: stable("command.comfyui-test").unwrap(),
        node_id: stable("agent.1").unwrap(),
        workspace: projects.resolve_workspace_v1(&workspace).unwrap(),
        project_branch: None,
        bindings: vec![tool],
        deadline_epoch_millis: u64::MAX,
        model_gateway: None,
        model_binding_id: None,
        model_version_hash: None,
        model_context: serde_json::json!({}),
        maximum_tool_output_bytes: MAXIMUM_TOOL_RESULT_BYTES,
        mcp_manifests: BTreeMap::new(),
        cancellation: CancellationToken::default(),
    });
    let outer = stable("invocation.comfyui-test").unwrap();
    let cancel = CancellationToken::default();
    // Validation admits the same call shape the model would propose.
    assert_eq!(
        authority
            .prepare_context(&outer, 0, &definitions, &cancel)
            .unwrap()
            .len(),
        0
    );
    let call = ModelToolCallV1 {
        call_id: "comfyui.krea.1".into(),
        provider_call_id: Some("comfyui.krea.1".into()),
        capability_id: COMFYUI_TOOL.into(),
        name: "krea_image".into(),
        arguments: json!({"prompt": "a quiet harbour"}),
        provider_context: None,
    };
    let settled = authority.invoke(&outer, 0, &call, &cancel).unwrap();
    assert!(!settled.result.is_error, "{:?}", settled.result.content);
    assert_eq!(
        settled.result.content["source"]["promptId"],
        json!("fixture-prompt")
    );
    assert_eq!(
        settled.result.content["source"]["workflowPath"],
        json!(workflow_path.to_string_lossy())
    );
    // The produced image is stored as immutable evidence and reaches the model.
    assert_eq!(settled.result.images.len(), 1);
    let image = &settled.result.images[0];
    assert_eq!(image.name, "krea_00001_.png");
    assert_eq!(image.mime_type, "image/png");
    assert!(image.byte_length > 0);
    assert!(root
        .path()
        .join("images")
        .join(&image.id)
        .is_file());

    // An undeclared argument is refused as a tool error, never executed.
    let bad = ModelToolCallV1 {
        call_id: "comfyui.krea.2".into(),
        provider_call_id: Some("comfyui.krea.2".into()),
        arguments: json!({"prompt": "a quiet harbour", "bogus": true}),
        ..call.clone()
    };
    let refused = authority.invoke(&outer, 1, &bad, &cancel).unwrap();
    assert!(refused.result.is_error);
    assert!(refused.result.images.is_empty());
    server.join().unwrap();
}
