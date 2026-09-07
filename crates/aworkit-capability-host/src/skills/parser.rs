//! Open YAML frontmatter with fail-closed invocation controls.

use super::is_skill_name;
use serde_yaml_ng::{Mapping, Value};

pub(super) struct ParsedSkill {
    pub name: String,
    pub description: String,
    pub model_invocable: bool,
    pub user_invocable: bool,
    pub body: String,
}

pub(super) fn parse(raw: &str) -> Result<ParsedSkill, String> {
    let (first, rest) = raw.split_once('\n').ok_or("missing YAML frontmatter")?;
    if first.trim_end_matches('\r') != "---" {
        return Err("missing YAML frontmatter".into());
    }
    let mut end = 0;
    let (yaml, body) = loop {
        let line = rest[end..]
            .split_inclusive('\n')
            .next()
            .ok_or("missing YAML frontmatter terminator")?;
        if line.trim_end_matches('\n').trim_end_matches('\r') == "---" {
            break (&rest[..end], &rest[end + line.len()..]);
        }
        end += line.len();
    };
    let data: Mapping =
        serde_yaml_ng::from_str(yaml).map_err(|e| format!("invalid YAML frontmatter: {e}"))?;
    let name = required_text(&data, "name")?;
    let description = required_text(&data, "description")?;
    if !is_skill_name(&name) {
        return Err(format!("invalid skill name \"{name}\""));
    }
    for legacy in ["disableModelInvocation", "modelInvocable", "userInvocable"] {
        if data.contains_key(Value::String(legacy.into())) {
            return Err(format!(
                "unsupported invocation frontmatter field \"{legacy}\""
            ));
        }
    }
    Ok(ParsedSkill {
        name,
        description,
        model_invocable: boolean(&data, "disable-model-invocation")? != Some(true),
        user_invocable: boolean(&data, "user-invocable")? != Some(false),
        body: body.trim().into(),
    })
}

fn required_text(data: &Mapping, key: &str) -> Result<String, String> {
    data.get(Value::String(key.into()))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("frontmatter requires {key}"))
}

fn boolean(data: &Mapping, key: &str) -> Result<Option<bool>, String> {
    let Some(value) = data.get(Value::String(key.into())) else {
        return Ok(None);
    };
    let parsed = match value {
        Value::Bool(v) => Some(*v),
        Value::Number(v) if v.as_f64() == Some(1.0) => Some(true),
        Value::Number(v) if v.as_f64() == Some(0.0) => Some(false),
        Value::String(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    };
    parsed
        .map(Some)
        .ok_or_else(|| format!("frontmatter field \"{key}\" must be a boolean"))
}
