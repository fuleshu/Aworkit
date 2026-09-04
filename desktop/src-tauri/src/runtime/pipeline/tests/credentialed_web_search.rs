//! Durable workflow coverage for a credential-backed web-search binding.

use std::{fs, path::Path};

use aworkit_capability_host::{WebSearchResultV1, WebTools, WebTransportPort};
use crate::runtime::tool_loop::WorkflowToolCredentialBindingV1;

use super::*;

#[test]
fn credentialed_search_crosses_host_returns_failure_to_model_and_replays_opaquely() {
    let root = TempDir::new().expect("root");
    let (mut pipeline, credential_store, provider_credential, calls, observed_results) =
        setup_tool_pipeline(&root, ToolScriptV1::WebSearch);
    pipeline
        .file_tool_authority
        .set_web_tools_for_test(WebTools::new(Arc::new(NoNetworkWebTransport)));
    let mut broker = SecretBroker::with_store(credential_store);
    let tool_credential = broker
        .put_credential(
            CredentialRef(stable("credential.web-search-pipeline-test").expect("credential ID")),
            BTreeMap::from([(
                API_KEY_FIELD.to_owned(),
                b"tool-search-secret".to_vec(),
            )]),
        )
        .expect("tool credential");

    let mut configuration = aworkit_capability_host::WebSearchConfigurationV1::default();
    configuration.backend = aworkit_capability_host::WebSearchBackendV1::Deepseek;
    configuration.credential_backend = aworkit_capability_host::WebSearchBackendV1::Deepseek;
    let mut execution = request(provider_credential);
    execution.tools = vec![WorkflowToolBindingV1 {
        capability_id: WEB_SEARCH_CAPABILITY_ID.into(),
        configuration: serde_json::to_value(configuration).expect("web-search configuration"),
        credential_bindings: vec![WorkflowToolCredentialBindingV1 {
            name: API_KEY_FIELD.into(),
            credential_ref: tool_credential.credential.0.clone(),
            field: API_KEY_FIELD.into(),
            field_names: tool_credential.field_names.clone(),
            revision: tool_credential.revision,
        }],
        definition: None,
    }];
    execution.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] =
        json!([WEB_SEARCH_CAPABILITY_ID]);

    let first = pipeline
        .execute(execution.clone())
        .expect("credentialed workflow execution");
    assert_eq!(
        first.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        first.error
    );
    assert_eq!((first.model_turns, first.tool_calls), (2, 1));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let observed = observed_results.lock().expect("observed tool results");
    assert_eq!(observed.len(), 1);
    assert!(
        observed[0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("injected test transport")),
        "credentialed search must reach the dispatcher and return its settled failure: {observed:?}"
    );
    drop(observed);

    let prepared = pipeline
        .records
        .execution(&execution.request_id)
        .expect("prepared record read")
        .expect("prepared record");
    let durable = serde_json::to_value(&prepared.tool_bindings[0])
        .expect("durable credentialed tool binding");
    assert!(durable.get("opaqueBinding").is_some());
    assert!(durable.get("secret").is_none());

    let replay = pipeline.execute(execution).expect("durable replay");
    assert!(replay.replayed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_files_exclude(root.path(), b"tool-search-secret");
}

struct NoNetworkWebTransport;

impl WebTransportPort for NoNetworkWebTransport {
    fn search(
        &self,
        _query: &str,
        _maximum_results: usize,
    ) -> Result<Vec<WebSearchResultV1>, String> {
        Err("test transport must not perform credentialed search".into())
    }

    fn fetch(
        &self,
        _url: &str,
        _maximum_download_bytes: usize,
    ) -> Result<(String, String, u64), String> {
        Err("test transport must not fetch".into())
    }
}

fn assert_files_exclude(root: &Path, forbidden: &[u8]) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).expect("profile directory") {
            let path = entry.expect("profile entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = fs::read(path).expect("profile file");
                assert!(
                    !bytes
                        .windows(forbidden.len())
                        .any(|window| window == forbidden),
                    "workflow profile persisted tool credential material"
                );
            }
        }
    }
}
