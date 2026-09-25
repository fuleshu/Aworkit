//! Actionable diagnostics for a `files.edit` call that matched nothing.
//!
//! "old_string was not found in the file" is true but robs the model of every
//! fact the tool already holds: the file it just read, its current size and
//! hash, whether the content changed since this Run last saw it, and whether the
//! quoted text is present modulo line endings or indentation. Those facts decide
//! the next move — re-read a stale copy, re-quote the whitespace, or accept that
//! the text is simply wrong — so a miss reports them instead of a dead end.
//! The message is read inside a bounded tool-error field, so it stays terse and
//! puts the cause before the supporting detail.
use super::*;

/// The plain miss, kept for the case where nothing else is worth saying: the
/// file is byte-identical to what this Run last observed.
const NOT_FOUND: &str = "old_string was not found in the file";

/// Longest excerpt of the searched text echoed back to the model.
const EXCERPT_CHARS: usize = 40;
/// Similarity below which a line is not offered as the closest candidate.
const CANDIDATE_SIMILARITY: f64 = 0.6;
/// Longest current line offered as a candidate.
const CANDIDATE_CHARS: usize = 60;

/// One durable file observation the failing edit can compare against.
pub(super) struct EditMissContext<'a> {
    pub path: &'a str,
    pub current_bytes: usize,
    pub current_hash: &'a str,
    /// The content this Run last read, edited or wrote for the same file.
    pub observed: Option<FileObservationV1>,
}

/// Builds the failure message for one edit whose `old_string` matched nothing.
pub(super) fn describe_miss(text: &str, old_string: &str, context: &EditMissContext<'_>) -> String {
    // A normalised match is the most actionable outcome: the text is present,
    // only the quote's shape is wrong, so the model must not be told "not found".
    if let Some(difference) = normalised_difference(text, old_string) {
        return format!(
            "old_string does not match {path} byte for byte, but the same text is present {difference}. \
             The search started with {excerpt}. Re-quote the exact current text (the file is {bytes} bytes with content hash {hash}) and retry.",
            path = context.path,
            difference = difference.describe(),
            excerpt = excerpt(old_string),
            bytes = context.current_bytes,
            hash = context.current_hash,
        );
    }
    let mut message = NOT_FOUND.to_owned();
    match &context.observed {
        Some(observed) if observed.content_hash != context.current_hash => {
            message.push_str(&format!(
                " The file changed since this Run last read or edited it: then {was}{was_bytes}, now {now} ({now_bytes} bytes). \
                 Re-read it and apply the edit to the current content. The search started with {excerpt}.",
                was = observed.content_hash,
                was_bytes = observed
                    .bytes
                    .map(|bytes| format!(" ({bytes} bytes)"))
                    .unwrap_or_default(),
                now = context.current_hash,
                now_bytes = context.current_bytes,
                excerpt = excerpt(old_string),
            ));
            if let Some((line, content)) = nearest_candidate(text, old_string) {
                message.push_str(&format!(" Closest current line {line}: {content}"));
            }
        }
        // The file is byte-identical to what the Run last observed, so the quote
        // is simply wrong; the plain miss is the honest, unembellished answer.
        Some(_) => {}
        None => {
            message.push_str(&format!(
                " This Run has no earlier read or edit of it to compare with; the current file is {bytes} bytes with content hash {hash}. \
                 The search started with {excerpt}.",
                bytes = context.current_bytes,
                hash = context.current_hash,
                excerpt = excerpt(old_string),
            ));
            if let Some((line, content)) = nearest_candidate(text, old_string) {
                message.push_str(&format!(" Closest current line {line}: {content}"));
            }
        }
    }
    message
}

/// How the quoted text differs from the file when shape is normalised away.
enum NormalisedDifference {
    LineEndings,
    Indentation,
}

impl NormalisedDifference {
    fn describe(&self) -> &'static str {
        match self {
            Self::LineEndings => "after normalising CRLF or lone CR line endings to LF",
            Self::Indentation => "after ignoring leading whitespace",
        }
    }
}

/// The first way the text matches after normalisation, if it matches at all.
fn normalised_difference(text: &str, old_string: &str) -> Option<NormalisedDifference> {
    if (text.contains('\r') || old_string.contains('\r'))
        && normalise_line_endings(text).contains(&normalise_line_endings(old_string))
    {
        return Some(NormalisedDifference::LineEndings);
    }
    if without_leading_whitespace(text).contains(&without_leading_whitespace(old_string)) {
        return Some(NormalisedDifference::Indentation);
    }
    None
}

fn normalise_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `str::lines` already drops a trailing CR, so only leading whitespace is
/// normalised here; the caller checks line endings before this.
fn without_leading_whitespace(text: &str) -> String {
    text.lines()
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bounded, quoted first line of the searched text, so the model can see what
/// the tool actually looked for.
fn excerpt(old_string: &str) -> String {
    let first = old_string
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let truncated: String = first.chars().take(EXCERPT_CHARS).collect();
    if truncated.is_empty() {
        "\"\"".to_owned()
    } else if truncated.chars().count() < first.chars().count() {
        format!("\"{truncated}…\"")
    } else {
        format!("\"{truncated}\"")
    }
}

/// The current line that best matches the first quoted line, as `(line, text)`.
/// The first line is the usual near-match when only one detail changed, so the
/// model gets a concrete place to look instead of the whole file.
fn nearest_candidate(text: &str, old_string: &str) -> Option<(usize, String)> {
    let first = old_string
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let needle: String = first.chars().take(CANDIDATE_CHARS).collect();
    if needle.is_empty() {
        return None;
    }
    // An exact occurrence of the first line is the strongest candidate and its
    // position is unambiguous, even when the rest of the quote differs.
    if let Some(offset) = text.find(&needle) {
        let line = text[..offset].matches('\n').count() + 1;
        let content = text[offset..].lines().next().unwrap_or_default();
        return Some((line, bounded_line(content)));
    }
    let mut best: Option<(f64, usize, &str)> = None;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let similarity = similarity(&needle, trimmed);
        if similarity >= CANDIDATE_SIMILARITY && best.is_none_or(|(score, _, _)| similarity > score)
        {
            best = Some((similarity, index + 1, trimmed));
        }
    }
    best.map(|(_, line, content)| (line, bounded_line(content)))
}

fn bounded_line(line: &str) -> String {
    let truncated: String = line.chars().take(CANDIDATE_CHARS).collect();
    if truncated.chars().count() < line.chars().count() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// Shared prefix plus shared suffix over the longer line, a cheap bounded
/// similarity that catches a changed word or number in an otherwise equal line.
fn similarity(left: &str, right: &str) -> f64 {
    let left: Vec<char> = left.chars().take(CANDIDATE_CHARS).collect();
    let right: Vec<char> = right.chars().take(CANDIDATE_CHARS).collect();
    let prefix = left
        .iter()
        .zip(&right)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = left
        .iter()
        .rev()
        .zip(right.iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let longest = left.len().max(right.len()).max(1);
    (prefix + suffix).min(longest) as f64 / longest as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(content_hash: &str, bytes: u64) -> FileObservationV1 {
        FileObservationV1 {
            content_hash: content_hash.to_owned(),
            bytes: Some(bytes),
        }
    }

    fn hash(seed: char) -> String {
        format!("sha256:{}", seed.to_string().repeat(64))
    }

    fn context<'a>(current: &'a str, observed: Option<FileObservationV1>) -> EditMissContext<'a> {
        EditMissContext {
            path: "src/engine.rs",
            current_bytes: 42,
            current_hash: current,
            observed,
        }
    }

    #[test]
    fn a_changed_file_is_reported_as_a_stale_copy() {
        let message = describe_miss(
            "fn main() {}\n",
            "fn start() {}",
            &context(&hash('b'), Some(observation(&hash('a'), 40))),
        );
        assert!(message.starts_with(NOT_FOUND));
        assert!(
            message.contains("The file changed since this Run last read or edited it"),
            "{message}"
        );
        assert!(message.contains("(40 bytes)"), "{message}");
        assert!(message.contains("(42 bytes)"), "{message}");
        assert!(message.contains(&hash('a')), "{message}");
        assert!(message.contains(&hash('b')), "{message}");
    }

    #[test]
    fn an_unchanged_file_keeps_the_plain_message() {
        let message = describe_miss(
            "fn main() {}\n",
            "fn start() {}",
            &context(&hash('a'), Some(observation(&hash('a'), 42))),
        );
        assert_eq!(message, NOT_FOUND);
    }

    #[test]
    fn a_crlf_only_mismatch_reports_the_normalised_match() {
        let message = describe_miss(
            "fn main() {\r\n    run();\r\n}\r\n",
            "fn main() {\n    run();\n}",
            &context(&hash('c'), Some(observation(&hash('c'), 26))),
        );
        assert!(
            message.contains("after normalising CRLF or lone CR line endings to LF"),
            "{message}"
        );
        assert!(!message.contains(NOT_FOUND), "{message}");
    }

    #[test]
    fn an_indentation_only_mismatch_reports_the_normalised_match() {
        let message = describe_miss(
            "fn main() {\n    run();\n}\n",
            "fn main() {\n  run();\n}",
            &context(&hash('d'), Some(observation(&hash('d'), 24))),
        );
        assert!(
            message.contains("after ignoring leading whitespace"),
            "{message}"
        );
        assert!(!message.contains(NOT_FOUND), "{message}");
    }

    #[test]
    fn a_wrong_quote_still_names_the_closest_current_line() {
        let text = "fn alpha() {\n    let value = 1;\n}\n\nfn beta() {\n    let value = 2;\n}\n";
        let message = describe_miss(
            text,
            "fn gamma() {\n    let value = 1;",
            &context(&hash('b'), Some(observation(&hash('a'), 70))),
        );
        assert!(
            message.contains("Closest current line 1: fn alpha() {"),
            "{message}"
        );
    }

    #[test]
    fn a_first_line_match_is_located_by_line_number() {
        let text = "one\ntwo\nthree\n";
        let message = describe_miss(text, "two\nand a half", &context(&hash('e'), None));
        assert!(message.contains("Closest current line 2: two"), "{message}");
        assert!(message.contains("no earlier read or edit"), "{message}");
    }

    #[test]
    fn every_message_fits_the_bounded_tool_error_field() {
        let long_line = "x".repeat(400);
        let cases = [
            describe_miss(
                &format!("{long_line}\n"),
                &format!("{long_line}tail\nand more"),
                &context(&hash('b'), Some(observation(&hash('a'), 400))),
            ),
            describe_miss(
                "short\n",
                "missing fully\nand more",
                &context(&hash('b'), None),
            ),
            describe_miss(
                "line\r\n",
                "line\n",
                &context(&hash('b'), Some(observation(&hash('b'), 6))),
            ),
        ];
        for message in cases {
            assert!(
                message.len() <= 512,
                "{} bytes exceeds the bounded tool error field: {message}",
                message.len()
            );
        }
    }
}
