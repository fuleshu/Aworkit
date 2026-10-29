//! Line-ending-aware text edits.
//!
//! A model that read a CRLF file almost never quotes it back with CRLF: the
//! provider re-emits the newline its tokenizer uses, and the JSON argument
//! carries a lone `\n`. A byte-exact search therefore rejects a quote that is
//! genuinely present in the file, and the turns that follow are spent
//! re-reading and re-quoting the same missing text — sometimes failing the same
//! way again. The same mismatch runs the other way after a successful edit:
//! splicing the model's LF lines into a CRLF file leaves it with mixed endings.
//!
//! Matching here ignores line-ending differences only. Indentation still has to
//! match, because it is semantic. The bytes written are the file's own
//! convention: every untouched byte stays exactly as it was, and the inserted
//! lines take the ending of the line they join, so an edit no longer rewrites
//! line endings the caller never asked to change.

/// The line ending an edit adopts when it writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    /// The spelling reported back to the model.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Lf => "lf",
            Self::Crlf => "crlf",
        }
    }
}

/// One applied edit and the facts worth reporting about it.
pub(super) struct Edit {
    /// The complete new file content.
    pub(super) bytes: Vec<u8>,
    /// The ending the inserted lines were written with.
    pub(super) line_ending: LineEnding,
    /// Whether the quote or the replacement had to be reconciled with the file.
    pub(super) reconciled_line_endings: bool,
}

/// The outcome of searching for one `old_string`.
pub(super) enum EditMatch {
    Applied(Edit),
    /// The text is absent even after normalising line endings.
    NotFound,
    /// More than one occurrence, so no edit can be chosen.
    Ambiguous,
}

/// Replaces one occurrence of `old_string`, ignoring line-ending differences.
///
/// An exact occurrence is applied first and without reading the rest of the
/// file. Only a quote that matches nothing byte for byte is compared again with
/// CRLF and lone CR normalised to LF, and that comparison must be unique too:
/// a normalised match that is ambiguous is rejected exactly like an exact one.
pub(super) fn replace_once(text: &str, old_string: &str, new_string: &str) -> EditMatch {
    match occurrences(text, old_string).as_slice() {
        [start] => {
            let ending = adoption_ending(text, *start);
            return EditMatch::Applied(splice(
                text,
                *start,
                *start + old_string.len(),
                new_string,
                ending,
                false,
            ));
        }
        [] => {}
        _ => return EditMatch::Ambiguous,
    }

    let normalised = normalise(text);
    let normalised_old = normalise(old_string);
    let normalised_matches = occurrences(&normalised, &normalised_old);
    let [start] = normalised_matches.as_slice() else {
        return if normalised_matches.is_empty() {
            EditMatch::NotFound
        } else {
            EditMatch::Ambiguous
        };
    };
    let start = *start;
    let end = start + normalised_old.len();
    // A normalised offset counts text bytes with the dropped CRs removed, so it
    // has to be walked back onto the original before anything is spliced.
    let (Some(original_start), Some(original_end)) =
        (original_offset(text, start), original_offset(text, end))
    else {
        return EditMatch::NotFound;
    };
    let ending = adoption_ending(text, original_start);
    EditMatch::Applied(splice(
        text,
        original_start,
        original_end,
        new_string,
        ending,
        true,
    ))
}

/// The first two byte offsets where `needle` occurs.
///
/// Only "none, one or many" decides the edit, so the scan stops at the second
/// match instead of collecting every occurrence in a large file.
fn occurrences(haystack: &str, needle: &str) -> Vec<usize> {
    haystack
        .match_indices(needle)
        .take(2)
        .map(|(offset, _)| offset)
        .collect()
}

/// Writes one replacement and keeps every byte outside it untouched.
fn splice(
    text: &str,
    start: usize,
    end: usize,
    new_string: &str,
    ending: LineEnding,
    quote_was_normalised: bool,
) -> Edit {
    let adapted = reconcile(new_string, ending);
    let mut bytes = Vec::with_capacity(text.len() - (end - start) + adapted.len());
    bytes.extend_from_slice(&text.as_bytes()[..start]);
    bytes.extend_from_slice(adapted.as_bytes());
    bytes.extend_from_slice(&text.as_bytes()[end..]);
    let reconciled_line_endings = quote_was_normalised || adapted != new_string;
    Edit {
        bytes,
        line_ending: ending,
        reconciled_line_endings,
    }
}

/// CRLF and lone CR both become LF, so two texts compare by content alone.
fn normalise(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Rewrites `text` into `ending`, whatever newlines it arrived with.
fn reconcile(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => normalise(text),
        LineEnding::Crlf => normalise(text).replace('\n', "\r\n"),
    }
}

/// The ending inserted lines adopt at `start`: the ending that already ends the
/// line the edit writes into, or the file's own majority when the file has no
/// line break to follow.
fn adoption_ending(text: &str, start: usize) -> LineEnding {
    ending_at(text, start).unwrap_or_else(|| dominant_ending(text))
}

/// The line ending of the first line break at or after `from`.
fn ending_at(text: &str, from: usize) -> Option<LineEnding> {
    let bytes = text.as_bytes();
    let index = from + text.get(from..)?.find('\n')?;
    Some(if index > 0 && bytes[index - 1] == b'\r' {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    })
}

/// The ending most line breaks in `text` use; ties go to CRLF so a CRLF file
/// with a stray LF line is still treated as a CRLF file.
fn dominant_ending(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
    let mut crlf = 0usize;
    let mut lf = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        if index > 0 && bytes[index - 1] == b'\r' {
            crlf += 1;
        } else {
            lf += 1;
        }
    }
    if crlf > 0 && crlf >= lf {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

/// The offset in the original text that matches `target` normalised bytes.
///
/// Normalisation only removes the CR of a CRLF pair, so the walk is exact and
/// allocation-free; `None` means the target is past the end.
fn original_offset(text: &str, target: usize) -> Option<usize> {
    if target == 0 {
        return Some(0);
    }
    let bytes = text.as_bytes();
    let mut normalised = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            // The pair collapses into the single '\n' that follows it.
            index += 1;
            continue;
        }
        normalised += 1;
        index += 1;
        if normalised == target {
            return Some(index);
        }
    }
    (normalised == target).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The applied edit, so assertions read as the file does.
    fn applied(result: EditMatch) -> Edit {
        match result {
            EditMatch::Applied(edit) => edit,
            EditMatch::NotFound => panic!("expected an applied edit, got a miss"),
            EditMatch::Ambiguous => panic!("expected an applied edit, got ambiguity"),
        }
    }

    #[test]
    fn an_exact_quote_is_applied_without_touching_other_line_endings() {
        let edit = applied(replace_once("a\r\nb\r\n", "a\r\nb", "A\r\nB"));
        assert_eq!(edit.bytes, b"A\r\nB\r\n");
        assert_eq!(edit.line_ending, LineEnding::Crlf);
        assert!(!edit.reconciled_line_endings);
    }

    #[test]
    fn an_lf_quote_edits_a_crlf_file_and_keeps_crlf() {
        let edit = applied(replace_once(
            "alpha\r\nbeta\r\ngamma\r\n",
            "alpha\nbeta",
            "one\ntwo",
        ));
        assert_eq!(edit.bytes, b"one\r\ntwo\r\ngamma\r\n");
        assert!(edit.reconciled_line_endings);
    }

    #[test]
    fn a_crlf_quote_edits_an_lf_file_and_keeps_lf() {
        let edit = applied(replace_once("alpha\nbeta\n", "alpha\r\nbeta", "one\r\ntwo"));
        assert_eq!(edit.bytes, b"one\ntwo\n");
        assert!(edit.reconciled_line_endings);
    }

    #[test]
    fn a_single_line_edit_adopts_the_ending_of_the_line_it_joins() {
        // The file starts with one stray LF line; the edited line is CRLF, so
        // the inserted line follows its own line rather than the majority.
        let edit = applied(replace_once("intro\nvalue\r\nend\r\n", "value", "a\nb"));
        assert_eq!(edit.bytes, b"intro\na\r\nb\r\nend\r\n");
        assert!(edit.reconciled_line_endings);
        assert_eq!(edit.line_ending, LineEnding::Crlf);
    }

    #[test]
    fn an_edit_at_the_end_of_a_crlf_file_adopts_the_file_ending() {
        let edit = applied(replace_once("alpha\r\nbeta", "beta", "one\ntwo"));
        assert_eq!(edit.bytes, b"alpha\r\none\r\ntwo");
        assert_eq!(edit.line_ending, LineEnding::Crlf);
    }

    #[test]
    fn an_ambiguous_normalised_match_is_still_rejected() {
        assert!(matches!(
            replace_once("a\r\nb\r\na\r\nb\r\n", "a\nb", "x"),
            EditMatch::Ambiguous
        ));
    }

    #[test]
    fn text_absent_after_normalising_is_a_miss() {
        assert!(matches!(
            replace_once("alpha\r\nbeta\r\n", "alpha\ngamma", "x"),
            EditMatch::NotFound
        ));
    }

    #[test]
    fn indentation_is_not_normalised_away() {
        assert!(matches!(
            replace_once("    alpha\r\n", "\talpha", "x"),
            EditMatch::NotFound
        ));
    }

    #[test]
    fn the_offset_walk_lands_on_the_original_bytes() {
        // "a\r\nb\rc\nd" normalises to "a\nb\nc\nd"; the rewritten offsets are
        // the starts of 'b', 'c' and of the text after the final 'd'.
        let text = "a\r\nb\rc\nd";
        assert_eq!(original_offset(text, 0), Some(0));
        assert_eq!(original_offset(text, 2), Some(3));
        assert_eq!(original_offset(text, 4), Some(5));
        assert_eq!(original_offset(text, 7), Some(8));
        assert_eq!(original_offset(text, 8), None);
    }

    #[test]
    fn the_dominant_ending_ignores_a_stray_line() {
        assert_eq!(dominant_ending("a\r\nb\r\nc\n"), LineEnding::Crlf);
        assert_eq!(dominant_ending("a\nb\nc\r\n"), LineEnding::Lf);
        assert_eq!(dominant_ending("single line"), LineEnding::Lf);
    }

    #[test]
    fn a_normalised_edit_keeps_surrounding_content_byte_for_byte() {
        let edit = applied(replace_once(
            "head\r\n  target();\r\ntail\r\n",
            "  target();",
            "  replacement();",
        ));
        assert_eq!(edit.bytes, b"head\r\n  replacement();\r\ntail\r\n");
    }
}
