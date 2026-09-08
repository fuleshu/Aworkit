//! Lossless projection of MCP results into the model's continuation payload.

use serde_json::Value;

/// MCP servers can return structured data and a serialized text copy for older
/// clients. Keep the structured value once; retain all distinct text, annotated
/// blocks, media and metadata. Compare parsed values, never approximate text.
pub(crate) fn model_result(mut result: Value) -> Value {
    let Some(object) = result.as_object_mut() else {
        return result;
    };
    let Some(structured) = object
        .get("structuredContent")
        .filter(|v| v.is_object())
        .cloned()
    else {
        return result;
    };
    if let Some(content) = object.get_mut("content").and_then(Value::as_array_mut) {
        content.retain(|block| {
            let Some(block) = block.as_object() else {
                return true;
            };
            !(block.len() == 2
                && block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .is_some_and(|value| value == structured))
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duplicated_memory_fits_without_losing_any_structured_data() {
        let memory = json!({"projectName":"Aworkit","memory":{"body":"m".repeat(270_000)}});
        let original = json!({"isError":false,"structuredContent":memory,
            "content":[{"type":"text","text":serde_json::to_string_pretty(&memory).unwrap()}]});
        assert!(serde_json::to_vec(&original).unwrap().len() > 512 * 1024);
        let projected = model_result(original);
        assert!(serde_json::to_vec(&projected).unwrap().len() < 300_000);
        assert_eq!(projected["structuredContent"], memory);
        assert_eq!(projected["isError"], false);
        assert_eq!(projected["content"], json!([]));
    }

    #[test]
    fn preserves_distinct_blocks_metadata_and_errors() {
        let result = json!({"isError":true,"structuredContent":{"message":"failed"},
            "content":[{"type":"text","text":"explanation"},
                {"type":"text","text":"{\"message\":\"different\"}"},
                {"type":"text","text":"{\"message\":\"failed\"}","annotations":{"audience":["user"]}},
                {"type":"image","data":"image","mimeType":"image/png"}],
            "_meta":{"source":"fixture"}});
        assert_eq!(model_result(result.clone()), result);
        let text_only = json!({"content":[{"type":"text","text":"{\"value\":1}"}]});
        assert_eq!(model_result(text_only.clone()), text_only);
    }
}
