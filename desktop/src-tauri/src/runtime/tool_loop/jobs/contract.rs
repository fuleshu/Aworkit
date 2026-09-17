//! Installed job schemas and frozen authority mapping.
use super::super::*;
pub const START: &str = "tool.shell.start";
pub const PYTHON_START: &str = "tool.python.start";
pub const OUTPUT: &str = "tool.job.output";
pub const INPUT: &str = "tool.job.input";
pub const STOP: &str = "tool.job.stop";
pub const LIST: &str = "tool.job.list";
pub const KEEP: &str = "tool.job.keep";
pub const IDS: [&str; 7] = [START, OUTPUT, INPUT, STOP, LIST, KEEP, PYTHON_START];
pub fn is_job(id: &str) -> bool {
    IDS.contains(&id)
}
pub fn controls_available(bindings: &[StoredFileToolBindingV1]) -> bool {
    [OUTPUT, INPUT, STOP, LIST]
        .iter()
        .all(|id| bindings.iter().any(|binding| binding.capability_id == *id))
}
pub fn schema(id: &str) -> Value {
    super::super::super::tool_registry::native_tool(id)
        .expect("installed job tool")
        .input_schema
        .clone()
}
pub fn freeze(
    id: &str,
    configuration: &Value,
) -> Result<(String, String, Value, StoredFileToolLimitV1), WorkflowPipelineError> {
    if configuration != &json!({}) {
        return Err(invalid_tool("job configuration must be empty"));
    }
    let native = super::super::super::tool_registry::native_tool(id)
        .ok_or_else(|| invalid_tool("unknown job tool"))?;
    Ok((
        native.provider_name.clone(),
        native.description.clone(),
        native.input_schema.clone(),
        StoredFileToolLimitV1::Job {
            operation: native.provider_name.clone(),
        },
    ))
}
pub fn validate(operation: &str, args: &Value) -> Result<(), WorkflowPipelineError> {
    let object = args
        .as_object()
        .ok_or_else(|| invalid_tool("job arguments must be an object"))?;
    let (allowed, required): (&[&str], &[&str]) = match operation {
        "shell_start" => (&["command", "interactive"], &["command"]),
        "python_start" => (&["script", "interactive"], &["script"]),
        "job_output" => (&["jobId", "cursor", "waitMs", "maximumBytes"], &["jobId"]),
        "job_input" => (&["jobId", "text", "closeStdin"], &["jobId", "text"]),
        "job_stop" => (&["jobId"], &["jobId"]),
        "job_keep" => (&["jobId", "reason"], &["jobId", "reason"]),
        "job_list" => (&[], &[]),
        _ => return Err(invalid_tool("unknown job operation")),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key))
    {
        return Err(invalid_tool("invalid job argument keys"));
    }
    for (key, max, empty) in [
        ("jobId", 80, false),
        ("command", 262144, false),
        ("script", 262144, false),
        ("text", 16384, true),
        ("reason", 2048, false),
    ] {
        if let Some(value) = object.get(key)
            && !value.as_str().is_some_and(|s| {
                s.len() <= max && (empty || !s.trim().is_empty()) && !s.contains('\0')
            })
        {
            return Err(invalid_tool("invalid job text argument"));
        }
    }
    for key in ["interactive", "closeStdin"] {
        if object.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(invalid_tool("invalid job boolean"));
        }
    }
    for (key, min, max) in [("waitMs", 0, 60000), ("maximumBytes", 1, 262144)] {
        if object
            .get(key)
            .is_some_and(|v| !v.as_u64().is_some_and(|n| n >= min && n <= max))
        {
            return Err(invalid_tool("invalid job numeric bound"));
        }
    }
    if let Some(cursor) = object.get("cursor")
        && !cursor.as_object().is_some_and(|o| {
            o.len() == 2
                && o.get("stdout").is_some_and(Value::is_u64)
                && o.get("stderr").is_some_and(Value::is_u64)
        })
    {
        return Err(invalid_tool("invalid output cursor"));
    }
    Ok(())
}
