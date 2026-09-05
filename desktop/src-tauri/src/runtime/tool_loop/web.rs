//! Web tool orchestration: retain evidence first, then fit a truthful model preview.
// The shared workflow error contract contains durable approval state; keep that contract intact.
#![allow(clippy::result_large_err)]
use super::*;
use aworkit_capability_host::{WebDocumentV1, WebExtractionQualityV1};

impl FileToolDispatcherV1 {
    pub(super) fn run_web(
        &self,
        maximum_download: usize,
        maximum_extract: usize,
        allow_render: bool,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let scope = WebCancellation::new(cancellation, self.context.deadline_epoch_millis);
        let cancellation = &scope.token;
        let args = &self.record.call.arguments;
        let multi = self.record.binding.provider_name == WEB_EXTRACT_PROVIDER_NAME;
        let urls: Vec<&str> = if multi {
            args["urls"]
                .as_array()
                .ok_or("urls is invalid")?
                .iter()
                .map(|v| v.as_str().ok_or("url is invalid"))
                .collect::<Result<_, _>>()?
        } else {
            vec![args["url"].as_str().ok_or("url is invalid")?]
        };
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let requested = args["char_limit"]
            .as_u64()
            .unwrap_or(maximum_extract as u64) as usize;
        let maximum_extract = maximum_extract.min(requested);
        let budget = self
            .context
            .maximum_tool_output_bytes
            .min(MAXIMUM_TOOL_RESULT_BYTES);
        let mut pages = Vec::new();
        let mut used = 240usize;
        let mut skipped = Vec::new();
        for (index, url) in urls.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err("web request was cancelled".into());
            }
            // If even an evidence receipt cannot fit, leave the URL unrequested and say so.
            if budget.saturating_sub(used) < 700 {
                skipped.push(index);
                continue;
            }
            let document_id = args["documentId"].as_str();
            let fetched = match document_id {
                Some(id) => self
                    .runtime
                    .web_documents
                    .read(&self.context.run_id, url, id),
                None => self
                    .web
                    .document_v1(url, maximum_download, allow_render, cancellation)
                    .map_err(|e| e.to_string()),
            };
            if cancellation.is_cancelled() {
                return Err("web request was cancelled".into());
            }
            let page_budget = (budget - used).min(if index + 1 < urls.len() {
                (budget - used) / (urls.len() - index).min((budget - used) / 700).max(1)
            } else {
                budget - used
            });
            let mut page = match fetched {
                Ok(mut document) => {
                    let retained = match document_id {
                        Some(id) => Some(id.to_owned()),
                        None => {
                            match self
                                .runtime
                                .web_documents
                                .retain(&self.context.run_id, &document)
                            {
                                Ok(id) => Some(id),
                                Err(_) => {
                                    document.metadata.warnings.push("Could not retain this document; continuation is unavailable.".into());
                                    None
                                }
                            }
                        }
                    };
                    match preview(
                        &document,
                        retained.as_deref(),
                        offset,
                        maximum_extract,
                        page_budget.saturating_sub(24),
                    ) {
                        Ok(page) => page,
                        Err(error) if multi => {
                            json!({"status":"failed","error":error,"documentId":retained})
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) if !multi => return Err(error),
                Err(error) => {
                    json!({"status":"failed","error":utf8_prefix(&error, page_budget.saturating_sub(100).min(512))})
                }
            };
            page["index"] = json!(index);
            used += serde_json::to_vec(&page).map_err(|e| e.to_string())?.len() + 1;
            pages.push(page);
        }
        let usable = pages
            .iter()
            .filter(|p| matches!(p["status"].as_str(), Some("complete" | "partial")))
            .count();
        let partial = pages.iter().filter(|p| p["status"] == "partial").count();
        let failed = pages.len() - usable;
        let summary = if !multi && failed > 0 {
            match pages[0]["status"].as_str() {
                Some("needsRendering") => "Page needs JavaScript; rendering was unavailable or did not yield usable content.".into(),
                Some("blocked") => "Page returned an access challenge, sign-in request, or subscription gate.".into(),
                Some("empty") => "No readable content was found in the retrieved page.".into(),
                _ => "The web page could not be retrieved.".into(),
            }
        } else if failed == 0 && skipped.is_empty() {
            format!("Read {usable} web page(s); {partial} partial result(s).")
        } else {
            format!(
                "Read {usable} web page(s); {partial} partial, {failed} unavailable, {} not fetched within the output budget.",
                skipped.len()
            )
        };
        let value = if multi {
            json!({"results":pages,"notFetchedIndices":skipped,"notice":"For more text use documentId and nextOffset with the same URL in this Run. Unfetched indices refer to input URLs."})
        } else {
            let mut page = pages
                .pop()
                .ok_or("tool output budget is too small for a web receipt")?;
            if let Some(content) = page.as_object_mut().and_then(|page| page.remove("content")) {
                page["text"] = content;
            }
            page
        };
        if cancellation.is_cancelled() {
            return Err("web request was cancelled or its frozen deadline expired".into());
        }
        enforce_result_bound(&value)?;
        if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > budget {
            return Err("web receipt exceeded the frozen output budget".into());
        }
        Ok((value, summary))
    }
}

/// Links a call-local token to both user cancellation and the frozen Run deadline.
/// Finishing the call stops and joins the watcher, so no background timers accumulate.
struct WebCancellation {
    token: CancellationToken,
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl WebCancellation {
    fn new(parent: &CancellationToken, deadline: u64) -> Self {
        let token = CancellationToken::default();
        if parent.is_cancelled() || current_epoch_millis() >= deadline {
            token.cancel();
        }
        let (stop, receiver) = std::sync::mpsc::channel();
        let parent = parent.clone();
        let target = token.clone();
        let worker = std::thread::spawn(move || {
            loop {
                if parent.is_cancelled() || current_epoch_millis() >= deadline {
                    target.cancel();
                    break;
                }
                match receiver.recv_timeout(std::time::Duration::from_millis(25)) {
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
        });
        Self {
            token,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for WebCancellation {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn utf8_prefix(text: &str, maximum: usize) -> &str {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Metadata and continuation remain valid JSON even under the smallest model budget.
fn preview(
    document: &WebDocumentV1,
    id: Option<&str>,
    offset: usize,
    maximum: usize,
    budget: usize,
) -> Result<Value, String> {
    if offset > document.text.len() || !document.text.is_char_boundary(offset) {
        return Err("offset must be a UTF-8 byte boundary inside the saved document".into());
    }
    let mut length = maximum.min(document.text.len() - offset);
    let mut compact = false;
    loop {
        let content = utf8_prefix(&document.text[offset..], length);
        if content.is_empty()
            && offset < document.text.len()
            && length
                < document.text[offset..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8)
        {
            return Err("preview byte limit cannot fit the next UTF-8 character; increase char_limit or maximumExtractBytes".into());
        }
        let next = offset + content.len();
        let more = next < document.text.len();
        let source_partial = document.metadata.download_truncated
            || document.metadata.snapshot_truncated
            || document.metadata.document_truncated
            || document.metadata.render_settled == Some(false);
        let status = match document.metadata.quality {
            WebExtractionQualityV1::Usable if source_partial || more => "partial",
            WebExtractionQualityV1::Usable => "complete",
            WebExtractionQualityV1::NeedsRendering => "needsRendering",
            WebExtractionQualityV1::Blocked => "blocked",
            WebExtractionQualityV1::Empty => "empty",
        };
        let mut value = json!({"status":status,"documentId":id,"offset":offset,"nextOffset":if more && id.is_some() {Some(next)} else {None},
            "retainedBytes":document.text.len(),"previewTruncated":more,"downloadTruncated":document.metadata.download_truncated,
            "snapshotTruncated":document.metadata.snapshot_truncated,"documentTruncated":document.metadata.document_truncated,
            "renderSettled":document.metadata.render_settled,"method":document.metadata.method,"fetchedAtEpochMs":document.metadata.fetched_at_epoch_ms,
            "content":content,"continuationAvailable":id.is_some()});
        if !compact {
            value["url"] = json!(document.url);
            value["title"] = json!(document.title);
            value["metadata"] = json!(document.metadata);
            value["bytesDownloaded"] = json!(document.bytes_downloaded);
        } else {
            value["metadataOmittedForBudget"] = json!(true);
        }
        if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() <= budget {
            return Ok(value);
        }
        if !compact {
            compact = true;
            continue;
        }
        if length == 0 {
            return Err("tool output budget cannot fit a web document receipt".into());
        }
        length /= 2;
    }
}

pub(super) fn unavailable(value: &Value) -> bool {
    let available = |page: &Value| matches!(page["status"].as_str(), Some("complete" | "partial"));
    match value.get("results").and_then(Value::as_array) {
        Some(pages) => !pages.iter().any(available),
        None => !available(value),
    }
}

/// Optional setting defaults on for new bindings; old serialized bindings default off.
pub(super) fn freeze_web_configuration(
    configuration: &Value,
) -> Result<StoredFileToolLimitV1, WorkflowPipelineError> {
    let mut configuration = configuration.clone();
    let object = configuration
        .as_object_mut()
        .ok_or_else(|| invalid_tool("web configuration is invalid"))?;
    let render_when_needed = object
        .remove("renderWhenNeeded")
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| invalid_tool("renderWhenNeeded must be boolean"))
        })
        .transpose()?
        .unwrap_or(true);
    let limits = freeze_configuration(
        &configuration,
        &[],
        &[
            (
                "maximumDownloadBytes",
                1,
                WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1,
            ),
            ("maximumExtractBytes", 1, WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1),
        ],
    )?;
    Ok(StoredFileToolLimitV1::WebFetch {
        maximum_download_bytes: limits["maximumDownloadBytes"],
        maximum_extract_bytes: limits["maximumExtractBytes"],
        render_when_needed,
    })
}

pub(super) fn rendering_disabled(value: &bool) -> bool {
    !value
}

pub(super) fn validate_continuation(arguments: &Value) -> Result<(), WorkflowPipelineError> {
    if let Some(id) = arguments.get("documentId") {
        if !id
            .as_str()
            .is_some_and(super::super::web_documents::valid_id)
        {
            return Err(invalid_tool("documentId is invalid"));
        }
        if arguments["urls"]
            .as_array()
            .is_some_and(|urls| urls.len() != 1)
        {
            return Err(invalid_tool(
                "a saved document read requires exactly one URL",
            ));
        }
    }
    if let Some(offset) = arguments.get("offset")
        && (arguments.get("documentId").is_none()
            || offset
                .as_u64()
                .is_none_or(|n| n > aworkit_capability_host::MAXIMUM_WEB_DOCUMENT_BYTES as u64))
    {
        return Err(invalid_tool(
            "offset requires a saved document and a valid byte range",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_frozen_web_authority_keeps_rendering_disabled_and_round_trips() {
        let original = json!({"kind":"web_fetch","maximum_download_bytes":1048576,"maximum_extract_bytes":32768});
        let limit: StoredFileToolLimitV1 = serde_json::from_value(original.clone()).unwrap();
        assert!(matches!(
            limit,
            StoredFileToolLimitV1::WebFetch {
                render_when_needed: false,
                ..
            }
        ));
        assert_eq!(serde_json::to_value(limit).unwrap(), original);
        assert!(matches!(
            freeze_web_configuration(
                &json!({"maximumDownloadBytes":1048576,"maximumExtractBytes":32768})
            )
            .unwrap(),
            StoredFileToolLimitV1::WebFetch {
                render_when_needed: true,
                ..
            }
        ));
    }
    #[test]
    fn cancellation_scope_observes_parent_and_deadline_without_cancelling_parent() {
        let parent = CancellationToken::default();
        let expired = WebCancellation::new(&parent, current_epoch_millis().saturating_sub(1));
        assert!(expired.token.is_cancelled());
        assert!(!parent.is_cancelled());
        let child = WebCancellation::new(&parent, current_epoch_millis() + 1000);
        parent.cancel();
        let started = std::time::Instant::now();
        while !child.token.is_cancelled() && started.elapsed() < std::time::Duration::from_secs(1) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(child.token.is_cancelled());
    }
}
