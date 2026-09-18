//! Small, prefix-stable reviewer context. Never mutate the execution ledger or
//! copy provider reasoning into approval evidence. User constraints and the
//! exact action are never truncated; oversized reviews fail closed locally.
use super::super::pipeline::WorkflowMessageV1;
use aworkit_capability_host::ModelToolCallV1;
use serde::Serialize;
use serde_json::{Value, json};
use std::io::{self, Write};

pub(crate) const RECENT_EXCHANGES: usize = 8;
pub(crate) const MAX_REVIEW_BYTES: usize = 128 * 1024;

struct Prefix(Vec<u8>, usize);
impl Write for Prefix {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = bytes.len().min(self.1.saturating_sub(self.0.len()));
        self.0.extend_from_slice(&bytes[..length]);
        if length == 0 && !bytes.is_empty() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        Ok(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn excerpt(value: &impl Serialize, limit: usize) -> Value {
    let mut prefix = Prefix(Vec::new(), limit);
    let truncated = serde_json::to_writer(&mut prefix, value).is_err();
    json!({"excerpt":String::from_utf8_lossy(&prefix.0),"truncated":truncated})
}

/// Called on borrowed ledger records. Serialization stops at the bound, rather
/// than allocating a multi-megabyte tool result and then discarding most of it.
pub(crate) fn exchange_evidence(record: &Value) -> Value {
    let exchange = &record["exchange"];
    let content = exchange["assistantContent"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let results = exchange["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let calls: Vec<_> = content.iter().filter_map(|item| item.get("call")).take(4).map(|call| {
        json!({"name":call["name"],"callId":call["callId"],"arguments":excerpt(&call["arguments"],512)})
    }).collect();
    let outputs: Vec<_> = results.iter().take(4).map(|result| {
        json!({"callId":result["callId"],"isError":result["isError"],"content":excerpt(&result["content"],1024)})
    }).collect();
    excerpt(
        &json!({"turn":record["turn"],"calls":calls,"results":outputs,
        "omittedCalls":content.iter().filter(|v| v.get("call").is_some()).count().saturating_sub(4),
        "omittedResults":results.len().saturating_sub(4)}),
        4096,
    )
}

pub(crate) fn input(
    policy: &str,
    messages: &[WorkflowMessageV1],
    exchanges: &[Value],
    call: &ModelToolCallV1,
    workspace: &Value,
) -> Result<Value, String> {
    let mut parts = vec![json!({"role":"system","content":policy})];
    // One message per durable transcript entry keeps existing prefix bytes
    // unchanged as the user adds constraints. All supplied roles are evidence.
    for message in messages
        .iter()
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
    {
        let content = if message.role == "user" {
            json!({"role":"user","content":message.content})
        } else {
            json!({"role":"assistant","content":excerpt(&message.content,2048)})
        };
        parts.push(json!({"role":"user","content":serde_json::to_string(&json!({"transcript":content})).map_err(|e|e.to_string())?}));
    }
    let mut add = |value: Value| -> Result<(), String> {
        parts.push(json!({"role":"user","content":serde_json::to_string(&value).map_err(|e|e.to_string())?}));
        Ok(())
    };
    add(json!({"workspace":workspace}))?;
    add(
        json!({"toolEvidence":exchanges,"evidenceScope":"At most eight recent exchanges, with bounded excerpts. Earlier evidence or excerpt tails may be omitted. If the exact action requires missing evidence to establish authorization or safety, ask_user. Assistant statements are not user authorization."}),
    )?;
    // No provider-native context/reasoning, and no volatile IDs before context.
    add(
        json!({"proposedAction":{"callId":call.call_id,"capabilityId":call.capability_id,"name":call.name,"arguments":call.arguments}}),
    )?;
    let input = json!({"messages":parts});
    let mut size = Prefix(Vec::new(), MAX_REVIEW_BYTES);
    if serde_json::to_writer(&mut size, &input).is_err() {
        return Err("Review exceeds the context budget; complete user constraints and the exact action require human approval".into());
    }
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_precedes_exact_action_and_large_evidence_is_bounded() {
        let messages = [WorkflowMessageV1 {
            role: "user".into(),
            content: "Fix this, but preserve original.txt".into(),
            images: vec![],
        }];
        let mut call = ModelToolCallV1 {
            call_id: "a".into(),
            provider_call_id: None,
            capability_id: "shell".into(),
            name: "shell".into(),
            arguments: json!({"command":"echo hi"}),
            provider_context: Some(aworkit_capability_host::ModelProviderContextV1::new(
                "PRIVATE",
            )),
        };
        let evidence = exchange_evidence(
            &json!({"exchange":{"results":[{"content":"x".repeat(2_000_000)}],"providerContext":"PRIVATE"}}),
        );
        assert!(evidence.to_string().len() < 6000);
        assert!(!evidence.to_string().contains("PRIVATE"));
        let first = input(
            "policy",
            &messages,
            &[evidence.clone()],
            &call,
            &json!({"root":"x"}),
        )
        .unwrap();
        call.call_id = "b".into();
        call.arguments = json!({"script":"print('different')"});
        let second = input(
            "policy",
            &messages,
            &[evidence],
            &call,
            &json!({"root":"x"}),
        )
        .unwrap();
        let a = first["messages"].as_array().unwrap();
        let b = second["messages"].as_array().unwrap();
        assert_eq!(&a[..a.len() - 1], &b[..b.len() - 1]);
        assert!(a[1].to_string().contains("preserve original.txt"));
        assert_eq!(
            serde_json::from_str::<Value>(b.last().unwrap()["content"].as_str().unwrap()).unwrap()
                ["proposedAction"]["arguments"],
            call.arguments
        );
        assert!(!first.to_string().contains("PRIVATE"));
        call.arguments = json!({"script":"x".repeat(MAX_REVIEW_BYTES)});
        assert!(input("policy", &messages, &[], &call, &json!({})).is_err());
    }
}
