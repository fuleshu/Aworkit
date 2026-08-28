//! Exact wire translation for client-side provider tools.

mod anthropic;
mod gemini;
mod openai;
mod openai_stream;

pub(crate) use anthropic::{anthropic_tool_request, normalize_anthropic_tool_response};
pub(crate) use gemini::{gemini_tool_request, normalize_gemini_tool_response};
pub(crate) use openai::{OpenAiRequestParametersV1, openai_tool_request};
pub(crate) use openai_stream::consume_openai_stream;
