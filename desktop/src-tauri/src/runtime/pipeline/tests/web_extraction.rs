//! Exercises real model/tool authority, saved continuation, and durable replay.
use super::*;
use aworkit_capability_host::{WebSearchResultV1, WebSourceV1, WebTools, WebTransportPort};
const CAPABILITY: &str = "tool.web_extract";

struct Fixture {
    downloads: Arc<AtomicUsize>,
}
impl WebTransportPort for Fixture {
    fn search(&self, _: &str, _: usize) -> Result<Vec<WebSearchResultV1>, String> {
        Ok(vec![])
    }
    fn fetch(&self, _: &str, _: usize) -> Result<(String, String, u64), String> {
        unreachable!()
    }
    fn fetch_document(
        &self,
        url: &str,
        _: usize,
        _: &CancellationToken,
    ) -> Result<WebSourceV1, String> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        if url.contains("broken") {
            return Err("fixture HTTP 503".into());
        }
        Ok(WebSourceV1 {
            final_url: url.into(),
            body: "αβγ evidence line.\n".repeat(2000),
            content_type: "text/plain".into(),
            bytes_downloaded: 8192,
            truncated: true,
            warning: None,
            title: Some("Fixture".into()),
        })
    }
}

struct Factory {
    observed: Arc<Mutex<Vec<Value>>>,
    many: bool,
}
impl ProviderFactoryV1 for Factory {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        _: &StoredProviderBindingV1,
        _: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        Ok(Box::new(Provider {
            binding: descriptor.capability_id.clone(),
            version: descriptor.version_hash.clone(),
            observed: self.observed.clone(),
            many: self.many,
        }))
    }
}
struct Provider {
    binding: String,
    version: String,
    observed: Arc<Mutex<Vec<Value>>>,
    many: bool,
}
impl ProviderEnginePortV1 for Provider {
    fn binding_id(&self) -> &str {
        &self.binding
    }
    fn version_hash(&self) -> &str {
        &self.version
    }
    fn execute(
        &self,
        _: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        emit(ModelEventV1::AssistantOutput("done".into()))?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        _: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        if request.exchanges.is_empty() {
            let urls = if self.many {
                (0..10)
                    .map(|i| {
                        if i == 1 {
                            "https://broken.example/".into()
                        } else {
                            format!("https://example.com/{i}")
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                vec!["https://example.com/".into()]
            };
            emit(tool_call(
                "call.web.first",
                CAPABILITY,
                "aworkit_web_extract",
                json!({"urls":urls}),
            ))?;
        } else if request.exchanges.len() == 1 && !self.many {
            let result = &request.exchanges[0].results[0].content;
            assert!(
                result.is_object(),
                "metadata was cut into a text prefix: {result}"
            );
            let page = &result["results"][0];
            assert_eq!(page["status"], "partial");
            assert!(page["documentId"].is_string(), "{page}");
            assert!(page["nextOffset"].as_u64().is_some_and(|n| n > 0), "{page}");
            emit(tool_call(
                "call.web.more",
                CAPABILITY,
                "aworkit_web_extract",
                json!({"urls":["https://example.com/"],"documentId":page["documentId"],"offset":page["nextOffset"]}),
            ))?;
        } else {
            self.observed.lock().unwrap().extend(
                request
                    .exchanges
                    .iter()
                    .flat_map(|e| e.results.iter().map(|r| r.content.clone())),
            );
            emit(ModelToolEventV1::AssistantOutput {
                text: "verified web evidence".into(),
            })?;
        }
        emit(ModelToolEventV1::Usage {
            input_tokens: 5,
            output_tokens: 3,
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

#[test]
fn web_extraction_small_model_budget_keeps_continuation_and_replay_does_not_download() {
    for (many, budget, storage_failure) in [
        (false, 1024, false),
        (true, 1024, false),
        (true, 64 * 1024, false),
        (true, 1024, true),
    ] {
        let root = TempDir::new().unwrap();
        let (mut pipeline, _, credential, _, _) =
            setup_tool_pipeline(&root, ToolScriptV1::WebSearch);
        let downloads = Arc::new(AtomicUsize::new(0));
        pipeline
            .file_tool_authority
            .set_web_tools_for_test(WebTools::new(Arc::new(Fixture {
                downloads: downloads.clone(),
            })));
        let observed = Arc::new(Mutex::new(vec![]));
        pipeline.provider_factory = Arc::new(Factory {
            observed: observed.clone(),
            many,
        });
        let mut execution = request(credential);
        execution.provider.maximum_tool_output_bytes = budget;
        if storage_failure {
            std::fs::write(
                root.path().join("history").join("web-documents"),
                b"fixture: storage unavailable",
            )
            .unwrap();
        }
        execution.tools = vec![WorkflowToolBindingV1 {
            capability_id: CAPABILITY.into(),
            configuration: json!({"maximumDownloadBytes":8192,"maximumExtractBytes":32768,"renderWhenNeeded":true}),
            credential_bindings: vec![],
            definition: None,
        }];
        execution.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] = json!([CAPABILITY]);
        let first = pipeline.execute(execution.clone()).unwrap();
        assert_eq!(
            first.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            first.error
        );
        assert_eq!(
            first.assistant_text.as_deref(),
            Some("verified web evidence")
        );
        let expected_downloads = if budget > 1024 { 10 } else { 1 };
        assert_eq!(
            downloads.load(Ordering::SeqCst),
            expected_downloads,
            "continuation re-fetched or unfetched URLs were still requested"
        );
        let observed = observed.lock().unwrap();
        for result in observed.iter() {
            assert!(serde_json::to_vec(result).unwrap().len() <= budget);
        }
        if many {
            assert_eq!(
                observed[0]["notFetchedIndices"].as_array().unwrap().len(),
                10 - expected_downloads
            );
            if budget > 1024 {
                assert_eq!(observed[0]["results"][1]["status"], "failed");
                assert_eq!(
                    observed[0]["results"][1]["error"],
                    "web request failed: fixture HTTP 503"
                );
            }
            if storage_failure {
                assert_eq!(observed[0]["results"][0]["continuationAvailable"], false);
                assert!(
                    observed[0]["results"][0]["content"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                );
            }
        } else {
            assert_eq!(observed.len(), 2);
            let first = &observed[0]["results"][0];
            let next = &observed[1]["results"][0];
            assert_eq!(first["documentId"], next["documentId"]);
            assert_eq!(first["nextOffset"], next["offset"]);
            assert_eq!(next["downloadTruncated"], true);
        }
        let replay = pipeline.execute(execution).unwrap();
        assert!(replay.replayed);
        assert_eq!(downloads.load(Ordering::SeqCst), expected_downloads);
    }
}
