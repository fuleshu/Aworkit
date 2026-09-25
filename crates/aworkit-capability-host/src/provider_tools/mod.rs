//! Exact wire translation for client-side provider tools.

mod anthropic;
#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod prompt_prefix_tests;
#[cfg(test)]
mod reasoning_passback_tests;
#[cfg(test)]
mod recorded_prefix_tests;
#[cfg(test)]
mod wire_size_tests;
mod gemini;
mod openai;
mod openai_stream;

pub(crate) use anthropic::{anthropic_tool_request, normalize_anthropic_tool_response};
pub(crate) use gemini::{gemini_tool_request, normalize_gemini_tool_response};
pub(crate) use openai::{OpenAiRequestParametersV1, openai_tool_request_body, prompt_first_body};
pub(crate) use openai_stream::consume_openai_stream;

/// Render positioned context with the same role/image mapping as base messages.
fn context_message(
    context: &crate::ModelToolContextV1,
    protocol: &str,
) -> Result<serde_json::Value, crate::ProviderError> {
    let message = crate::model_tools::normalize_context_message(&context.message())?;
    let role = if context.role.as_deref() == Some("assistant") {
        if protocol == "gemini" {
            "model"
        } else {
            "assistant"
        }
    } else {
        "user"
    };
    let content = crate::model_images::image_content(&message.content, &message.images, protocol)?;
    Ok(if protocol == "gemini" {
        serde_json::json!({"role":role,"parts":content})
    } else {
        serde_json::json!({"role":role,"content":content})
    })
}
