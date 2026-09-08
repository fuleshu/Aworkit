//! Frozen, versioned local transformation policy. No live settings or I/O.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off,
    #[default]
    Lossless,
    Adaptive,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tokenizer {
    #[default]
    Estimate,
    Cl100k,
    O200k,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub mode: Mode,
    pub minimum_bytes: usize,
    pub minimum_savings: f64,
    pub target_ratio: f64,
    pub tokenizer: Tokenizer,
    pub extract_code: bool,
    pub extract_prose: bool,
    pub protected_text: Vec<String>,
    pub excluded_tools: Vec<String>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: Mode::Lossless,
            minimum_bytes: 2048,
            minimum_savings: 0.15,
            target_ratio: 0.4,
            tokenizer: Tokenizer::Estimate,
            extract_code: true,
            extract_prose: true,
            protected_text: Vec::new(),
            excluded_tools: Vec::new(),
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if !(256..=524288).contains(&self.minimum_bytes)
            || !self.minimum_savings.is_finite()
            || !(0.05..=0.9).contains(&self.minimum_savings)
            || !self.target_ratio.is_finite()
            || !(0.1..=0.9).contains(&self.target_ratio)
            || self.protected_text.len() > 64
            || self.excluded_tools.len() > 128
            || self
                .protected_text
                .iter()
                .chain(&self.excluded_tools)
                .any(|s| s.is_empty() || s.len() > 1024 || s.contains('\0'))
        {
            return Err("Invalid compression policy: size 256–524288, savings 0.05–0.9, target 0.1–0.9, bounded nonempty protection/tool entries required.".into());
        }
        Ok(())
    }
}

/// Tokenizers are initialized once. The fallback is explicitly an estimate,
/// consistent with compaction's UTF-16 meter, never billed provider usage.
pub fn count(text: &str, tokenizer: Tokenizer) -> usize {
    use std::sync::OnceLock;
    static CL: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    static O: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    match tokenizer {
        Tokenizer::Estimate => text.encode_utf16().count().div_ceil(4),
        Tokenizer::Cl100k => CL
            .get_or_init(|| tiktoken_rs::cl100k_base().expect("embedded tokenizer"))
            .encode_ordinary(text)
            .len(),
        Tokenizer::O200k => O
            .get_or_init(|| tiktoken_rs::o200k_base().expect("embedded tokenizer"))
            .encode_ordinary(text)
            .len(),
    }
}
