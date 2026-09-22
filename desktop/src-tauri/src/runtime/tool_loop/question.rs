//! Model-facing questions: `tool.ask_user` and `tool.browse`.
//!
//! A question is not an executor. It suspends the Run through the same durable
//! challenge the approval path uses, and the user's answer is recorded before
//! the suspension resumes, so an answered question is delivered exactly once
//! and an unanswered one is never answered for the user.

use super::*;

/// Bound for the question prompt. Kept equal to the manifest schema bound.
pub(crate) const MAXIMUM_QUESTION_PROMPT_BYTES: usize = 8 * 1024;
/// Bound for the short dialog heading.
pub(crate) const MAXIMUM_QUESTION_TITLE_BYTES: usize = 256;
/// Bound for one option label.
const MAXIMUM_OPTION_LABEL_BYTES: usize = 256;
/// Bound for one option's consequence detail.
const MAXIMUM_OPTION_DESCRIPTION_BYTES: usize = 1024;
/// Bound for one option identity.
const MAXIMUM_OPTION_ID_BYTES: usize = 64;
/// Bound for a free-text answer.
pub(crate) const MAXIMUM_ANSWER_TEXT_BYTES: usize = 8 * 1024;
/// Manifest bound on the option list.
const MAXIMUM_OPTIONS: usize = 8;

/// What the user is being asked for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKindV1 {
    /// A labelled choice, optionally with a free-text answer.
    Choice,
    /// A file chosen from the user's own machine.
    File,
    /// A folder chosen from the user's own machine.
    Folder,
}

/// One labelled choice offered to the user.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionOptionV1 {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The durable question a suspended Run is waiting on.
///
/// It travels inside the existing approval challenge, so the suspension frame,
/// resume nonce and owner isolation are unchanged; the reason is simply a user
/// answer rather than an authority decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionChallengeV1 {
    pub kind: QuestionKindV1,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOptionV1>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_free_text: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_option_id: Option<String>,
    /// File extensions a browse question asked the chooser to show. A user
    /// choice is never restricted by them: they only narrow what is visible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

/// The user's answer to one question. A cancelled answer is an ordinary result:
/// the model continues instead of the Run failing or retrying.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionAnswerV1 {
    /// Chosen option, when the question offered labelled choices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    /// The user's own words, when the question allowed them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text: Option<String>,
    /// The path the user chose, for a browse question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The user skipped the question without answering it.
    #[serde(default)]
    pub cancelled: bool,
}

impl QuestionAnswerV1 {
    #[cfg(test)]
    pub(crate) fn cancelled() -> Self {
        Self {
            cancelled: true,
            ..Self::default()
        }
    }

    /// Whether the answer actually carries something the model can use.
    fn is_empty(&self) -> bool {
        !self.cancelled
            && self.option_id.is_none()
            && self
                .free_text
                .as_ref()
                .is_none_or(|text| text.trim().is_empty())
            && self.path.is_none()
    }
}

/// Validates one `tool.ask_user` call against the frozen argument contract.
pub(crate) fn validate_ask_user(
    arguments: &serde_json::Map<String, Value>,
) -> Result<(), WorkflowPipelineError> {
    bounded_prompt(arguments)?;
    if let Some(options) = arguments.get("options") {
        let options = options
            .as_array()
            .ok_or_else(|| invalid_tool("question options must be an array"))?;
        if options.is_empty() || options.len() > MAXIMUM_OPTIONS {
            return Err(invalid_tool(
                "a question offers between one and eight labelled options",
            ));
        }
        for option in options {
            let option = option
                .as_object()
                .ok_or_else(|| invalid_tool("question options must be objects"))?;
            let id = option
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= MAXIMUM_OPTION_ID_BYTES)
                .ok_or_else(|| invalid_tool("question option id is empty or oversized"))?;
            let _ = id;
            option
                .get("label")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= MAXIMUM_OPTION_LABEL_BYTES)
                .ok_or_else(|| invalid_tool("question option label is empty or oversized"))?;
            if let Some(description) = option.get("description") {
                bound_optional_text(
                    description,
                    MAXIMUM_OPTION_DESCRIPTION_BYTES,
                    "question option description",
                )?;
            }
            let keys = option.keys().map(String::as_str).collect::<BTreeSet<_>>();
            if !keys.is_subset(&BTreeSet::from(["id", "label", "description"])) {
                return Err(invalid_tool(
                    "question options accept exactly id, label, and description",
                ));
            }
        }
    }
    if let Some(default) = arguments.get("defaultOptionId") {
        bound_optional_text(default, MAXIMUM_OPTION_ID_BYTES, "default option id")?;
        let known = arguments
            .get("options")
            .and_then(Value::as_array)
            .is_some_and(|options| {
                options.iter().any(|option| {
                    option.get("id").and_then(Value::as_str)
                        == default.as_str()
                })
            });
        if !known {
            return Err(invalid_tool(
                "the default option must be one of the offered options",
            ));
        }
    }
    bound_optional_text(
        arguments.get("title").unwrap_or(&Value::Null),
        MAXIMUM_QUESTION_TITLE_BYTES,
        "question title",
    )
}

/// Validates one `tool.browse` call against the frozen argument contract.
pub(crate) fn validate_browse(
    arguments: &serde_json::Map<String, Value>,
) -> Result<(), WorkflowPipelineError> {
    bounded_prompt(arguments)?;
    arguments
        .get("kind")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "file" | "folder"))
        .ok_or_else(|| invalid_tool("browse kind must be file or folder"))?;
    bound_optional_text(
        arguments.get("title").unwrap_or(&Value::Null),
        MAXIMUM_QUESTION_TITLE_BYTES,
        "question title",
    )?;
    // The filter is only meaningful for a file choice.
    if arguments.get("kind").and_then(Value::as_str) == Some("folder")
        && arguments.contains_key("extensions")
    {
        return Err(invalid_tool("a folder question takes no extension filter"));
    }
    validate_extensions(arguments).map(|_| ())
}

/// Validates the optional file-extension filter of a browse question.
fn validate_extensions(
    arguments: &serde_json::Map<String, Value>,
) -> Result<Vec<String>, WorkflowPipelineError> {
    let Some(value) = arguments.get("extensions") else {
        return Ok(Vec::new());
    };
    let extensions = value
        .as_array()
        .ok_or_else(|| invalid_tool("browse extensions must be an array"))?;
    if extensions.len() > MAXIMUM_EXTENSIONS {
        return Err(invalid_tool("browse extensions exceed the supported count"));
    }
    extensions
        .iter()
        .map(|extension| {
            let text = extension
                .as_str()
                .filter(|text| !text.is_empty() && text.len() <= MAXIMUM_EXTENSION_BYTES)
                .ok_or_else(|| invalid_tool("a browse extension is empty or oversized"))?;
            if text.contains('.') || text.contains('\0') {
                return Err(invalid_tool(
                    "browse extensions are bare, for example csv",
                ));
            }
            Ok(text.to_owned())
        })
        .collect()
}

/// The durable question one question-tool call asks.
pub(crate) fn challenge(call: &ModelToolCallV1) -> Result<QuestionChallengeV1, String> {
    let object = call
        .arguments
        .as_object()
        .ok_or_else(|| "question arguments must be an object".to_owned())?;
    let prompt = object
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| "question prompt must be text".to_owned())?
        .to_owned();
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let kind = if call.capability_id == BROWSE_CAPABILITY_ID {
        if object.get("kind").and_then(Value::as_str) == Some("folder") {
            QuestionKindV1::Folder
        } else {
            QuestionKindV1::File
        }
    } else {
        QuestionKindV1::Choice
    };
    let options = object
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    Some(QuestionOptionV1 {
                        id: option.get("id")?.as_str()?.to_owned(),
                        label: option.get("label")?.as_str()?.to_owned(),
                        description: option
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let default_option_id = object
        .get("defaultOptionId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let extensions = object
        .get("extensions")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(QuestionChallengeV1 {
        kind,
        prompt,
        title,
        options,
        allow_free_text: object
            .get("allowFreeText")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        default_option_id,
        extensions,
    })
}

/// Validates one user answer against the question it answers, so a stale or
/// malformed answer can never be delivered as a question result.
pub(crate) fn validate_answer(
    question: &QuestionChallengeV1,
    answer: &QuestionAnswerV1,
) -> Result<(), String> {
    if answer.cancelled {
        if answer.option_id.is_some() || answer.path.is_some() {
            return Err("a cancelled question carries no answer".into());
        }
        return Ok(());
    }
    if answer.is_empty() {
        return Err("the answer is empty".into());
    }
    match question.kind {
        QuestionKindV1::Choice => {
            if answer.path.is_some() {
                return Err("a choice question is not answered with a path".into());
            }
            if let Some(option_id) = &answer.option_id
                && !question.options.iter().any(|option| &option.id == option_id)
            {
                return Err("the chosen option was not offered".into());
            }
            if answer.free_text.is_some() && !question.allow_free_text {
                return Err("this question does not accept a free-text answer".into());
            }
        }
        QuestionKindV1::File | QuestionKindV1::Folder => {
            if answer.option_id.is_some() || answer.free_text.is_some() {
                return Err("a path question is answered with a path".into());
            }
            let path = answer
                .path
                .as_deref()
                .ok_or_else(|| "a path question needs the chosen path".to_owned())?;
            if path.is_empty() || path.len() > MAXIMUM_PATH_BYTES || path.contains('\0') {
                return Err("the chosen path is empty, oversized, or malformed".into());
            }
        }
    }
    if let Some(text) = &answer.free_text
        && text.len() > MAXIMUM_ANSWER_TEXT_BYTES
    {
        return Err("the free-text answer is oversized".into());
    }
    Ok(())
}

/// Bound for a user-chosen path, matching the file tools.
const MAXIMUM_PATH_BYTES: usize = 4096;
/// Bound on the extension filter a browse question may request.
const MAXIMUM_EXTENSIONS: usize = 16;
/// Bound for one bare extension.
const MAXIMUM_EXTENSION_BYTES: usize = 16;

/// The tool result the model receives for one answered question.
pub(crate) fn result(
    question: &QuestionChallengeV1,
    answer: &QuestionAnswerV1,
) -> (Value, String) {
    if answer.cancelled {
        return (
            json!({
                "cancelled": true,
                "prompt": question.prompt,
                "detail": "The user skipped this question. Continue without that answer; do not ask it again unchanged.",
            }),
            "The user skipped the question.".to_owned(),
        );
    }
    match question.kind {
        QuestionKindV1::Choice => {
            let summary = answer
                .option_id
                .as_ref()
                .and_then(|id| question.options.iter().find(|option| &option.id == id))
                .map(|option| option.label.clone())
                .or_else(|| answer.free_text.clone())
                .unwrap_or_else(|| "Answered".to_owned());
            (
                json!({
                    "prompt": question.prompt,
                    "optionId": answer.option_id,
                    "freeText": answer.free_text,
                }),
                format!("The user answered: {summary}"),
            )
        }
        QuestionKindV1::File | QuestionKindV1::Folder => {
            let path = answer.path.clone().unwrap_or_default();
            (
                json!({
                    "prompt": question.prompt,
                    "path": path,
                    "detail": "The user chose this path. It is a user-supplied value, not a permission: the file tools still apply their own approval policy to it.",
                }),
                format!("The user chose {path}."),
            )
        }
    }
}

fn bounded_prompt(
    arguments: &serde_json::Map<String, Value>,
) -> Result<(), WorkflowPipelineError> {
    arguments
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.trim().is_empty()
                && value.len() <= MAXIMUM_QUESTION_PROMPT_BYTES
                && !value.contains('\0')
        })
        .ok_or_else(|| invalid_tool("question prompt is empty, oversized, or malformed"))?;
    Ok(())
}

fn bound_optional_text(
    value: &Value,
    maximum: usize,
    label: &str,
) -> Result<(), WorkflowPipelineError> {
    if value.is_null() {
        return Ok(());
    }
    value
        .as_str()
        .filter(|text| text.len() <= maximum && !text.contains('\0'))
        .ok_or_else(|| invalid_tool(&format!("{label} is oversized or malformed")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn arguments(value: Value) -> serde_json::Map<String, Value> {
        value.as_object().expect("object").clone()
    }

    fn call(capability_id: &str, value: Value) -> ModelToolCallV1 {
        ModelToolCallV1 {
            call_id: "call.question".into(),
            provider_call_id: Some("call.question".into()),
            capability_id: capability_id.into(),
            name: "browse".into(),
            arguments: value,
            provider_context: None,
        }
    }

    #[test]
    fn a_file_question_carries_a_bounded_extension_filter() {
        let request = arguments(json!({
            "prompt": "Which export should I read?",
            "kind": "file",
            "extensions": ["csv", "tsv"],
        }));
        validate_browse(&request).expect("valid file question");
        let asked = challenge(&call(BROWSE_CAPABILITY_ID, json!(request))).expect("challenge");
        assert_eq!(asked.kind, QuestionKindV1::File);
        assert_eq!(asked.extensions, vec!["csv", "tsv"]);

        // A folder question takes no filter, and a filter is never a disguised
        // path: extensions are bare, bounded and countable.
        assert!(
            validate_browse(&arguments(json!({
                "prompt": "Which folder?",
                "kind": "folder",
                "extensions": ["csv"],
            })))
            .is_err()
        );
        assert!(
            validate_browse(&arguments(json!({
                "prompt": "Which file?",
                "kind": "file",
                "extensions": [".csv"],
            })))
            .is_err()
        );
        assert!(
            validate_browse(&arguments(json!({
                "prompt": "Which file?",
                "kind": "file",
                "extensions": (0..17).map(|i| format!("e{i}")).collect::<Vec<_>>(),
            })))
            .is_err()
        );
        assert!(
            validate_browse(&arguments(json!({
                "prompt": "Which file?",
                "kind": "folder",
            })))
            .is_ok(),
            "a folder question without a filter stays valid"
        );
    }

    #[test]
    fn a_question_never_answers_itself() {
        // A cancelled answer carries nothing, and an empty or off-contract
        // answer is refused against the durable question.
        let asked = challenge(&call(
            ASK_USER_CAPABILITY_ID,
            json!({
                "prompt": "Which channel?",
                "options": [{"id":"stable","label":"Stable"}],
            }),
        ))
        .expect("challenge");
        assert_eq!(asked.kind, QuestionKindV1::Choice);
        assert_eq!(asked.extensions, Vec::<String>::new());
        assert!(validate_answer(&asked, &QuestionAnswerV1::cancelled()).is_ok());
        assert!(validate_answer(&asked, &QuestionAnswerV1::default()).is_err());
        assert!(
            validate_answer(
                &asked,
                &QuestionAnswerV1 {
                    free_text: Some("stable please".into()),
                    ..QuestionAnswerV1::default()
                }
            )
            .is_err(),
            "free text was not allowed by this question"
        );
        assert!(
            validate_answer(
                &asked,
                &QuestionAnswerV1 {
                    option_id: Some("stable".into()),
                    ..QuestionAnswerV1::default()
                }
            )
            .is_ok()
        );
    }
}
