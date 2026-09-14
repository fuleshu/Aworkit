//! Provider aliases are presentation identifiers, never MCP dispatch authority.
//!
//! The alias a model sees must be short, readable and stable, because a model
//! that cannot re-identify its own tool names hallucinates them. An opaque digest
//! prefix made every alias unmemorable, so the readable server label leads — and
//! only when the function name does not already name its own server, so the model
//! reads `adashi_design` rather than `adashi_adashi_design`.

use sha2::{Digest, Sha256};

/// Portable provider grammar ceiling for one tool name, in bytes.
const MAXIMUM_PROVIDER_NAME_BYTES: usize = 64;
/// Bytes reserved for the digest that keeps folded names distinct.
const DIGEST_SUFFIX_BYTES: usize = 8;
/// Bytes reserved for the readable server label inside a composed alias.
const MAXIMUM_SERVER_LABEL_BYTES: usize = 24;

/// Compose the provider-facing alias for one discovered MCP function.
///
/// `provider_label` is the MCP server's configured display name. The label is
/// prepended only when the function name does not already begin with it, so a
/// server named "Adashi" exposing `adashi_design` yields `adashi_design` and a
/// server exposing `read` yields `adashi_read`. A caller without a configured
/// name passes [`mcp_fallback_label`] of the server id instead.
///
/// The result always satisfies the provider contract: at most 64 bytes drawn from
/// `[A-Za-z0-9_-]`. A readable alias survives untouched; when folding or
/// truncation had to discard part of the identity — an over-long name, or two
/// configured labels that truncate alike — the alias closes with a digest of the
/// exact `(server, tool)` identity, so distinct functions never share one alias.
pub(crate) fn mcp_provider_name(server_id: &str, provider_label: &str, tool: &str) -> String {
    let label = server_label(server_id, provider_label);
    let (alias, alias_folded) = provider_alias(&label, tool);
    if !label.folded && !alias_folded && alias.len() <= MAXIMUM_PROVIDER_NAME_BYTES {
        return alias;
    }
    fold_to_provider_name(&alias, &identity_digest(server_id, tool))
}

/// The readable alias a model is asked to remember: the sanitized function name,
/// qualified by its server label only when the function name stands alone.
fn provider_alias(label: &FoldedServerLabel, tool: &str) -> (String, bool) {
    let folded = sanitize_identifier(tool);
    let alias = if names_its_own_server(&folded.value, &label.qualified)
        || names_its_own_server(&folded.value, &label.unqualified)
    {
        folded.value
    } else {
        format!("{}_{}", label.qualified, folded.value)
    };
    (alias, folded.folded)
}

/// Whether a function name already identifies the server it belongs to, so
/// qualifying it again would only repeat the label the model has to read.
///
/// The label must end at a separator inside the function name: `adashi_design`
/// names the server `adashi`, but `adashi` alone does not name itself, and a
/// function name shorter than the label can never start with it.
fn names_its_own_server(tool: &str, label: &str) -> bool {
    tool.len() >= label.len()
        && (tool == label
            || tool
                .strip_prefix(label)
                .is_some_and(|rest| rest.starts_with('_') || rest.starts_with('-')))
}

/// The two readings of one server's configured display name: the column an alias
/// may spend on it, and its full text. The full text is what decides whether a
/// function already names its own server; the capped text is what an alias is
/// qualified with.
struct FoldedServerLabel {
    qualified: String,
    unqualified: String,
    folded: bool,
}

/// Resolve the label an alias is qualified with: the configured display name when
/// it folds to something usable, otherwise the server's own id.
fn server_label(server_id: &str, provider_label: &str) -> FoldedServerLabel {
    let configured = sanitize_label(provider_label, provider_label.len());
    if configured.value.is_empty() {
        return FoldedServerLabel {
            qualified: mcp_fallback_label(server_id),
            unqualified: String::new(),
            folded: provider_label.trim().len() > MAXIMUM_SERVER_LABEL_BYTES,
        };
    }
    let truncated = configured.value.len() > MAXIMUM_SERVER_LABEL_BYTES;
    FoldedServerLabel {
        qualified: prefix_bytes(&configured.value, MAXIMUM_SERVER_LABEL_BYTES),
        unqualified: configured.value,
        folded: configured.folded || truncated,
    }
}

/// Close an alias with a digest of the identity that folding discarded. The
/// digest is what keeps distinct `(server, tool)` pairs distinct once their
/// readable form no longer carries the whole identity.
fn fold_to_provider_name(alias: &str, digest: &str) -> String {
    let digest = prefix_bytes(digest, DIGEST_SUFFIX_BYTES);
    let budget = MAXIMUM_PROVIDER_NAME_BYTES - digest.len() - 1;
    let folded = prefix_bytes(alias, budget);
    let folded = folded.trim_end_matches(['_', '-']);
    // A fold may land on a separator, so fall back to the untrimmed prefix rather
    // than emitting an alias with no readable stem at all.
    let stem = if folded.is_empty() {
        prefix_bytes(alias, budget)
    } else {
        folded.to_owned()
    };
    format!("{stem}_{digest}")
}

/// The readable label that stands in for a server whose configured display name
/// is unavailable. A generated server id such as
/// `mcp.166dddff4b6840dba8aed1edbbb9e427` yields `166dddff4b6840dba8aed1`: the
/// generated `mcp.` scheme prefix is not part of the label, and the remainder is
/// pure hex, so adjacent identifiers stay visually distinct.
pub(crate) fn mcp_fallback_label(server_id: &str) -> String {
    let trimmed = server_id
        .strip_prefix("mcp.")
        .or_else(|| server_id.strip_prefix("mcp_"))
        .unwrap_or(server_id);
    let label = sanitize_label(trimmed, MAXIMUM_SERVER_LABEL_BYTES).value;
    if label.is_empty() {
        "mcp".to_owned()
    } else {
        label
    }
}

/// Preserve aliases that may already appear in frozen provider exchanges.
/// Older overlong aliases could never pass request validation; repair only that
/// wire projection without changing the persisted definition or capability.
pub(crate) fn portable_frozen_name(server_id: &str, tool: &str, frozen: &str) -> String {
    if !frozen.is_empty() && is_portable_identifier(frozen) {
        frozen.to_owned()
    } else {
        mcp_provider_name(server_id, &mcp_fallback_label(server_id), tool)
    }
}

/// Whether an alias already satisfies the provider name contract.
fn is_portable_identifier(name: &str) -> bool {
    name.len() <= MAXIMUM_PROVIDER_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// A folded identifier, remembering whether folding or truncation discarded any
/// of the input so the caller knows the readable form no longer carries the
/// whole identity.
struct FoldedIdentifier {
    value: String,
    folded: bool,
}

/// Fold free-form configuration text into the portable grammar, keeping at most
/// `maximum_bytes` characters so the byte budget stays exact.
///
/// Legal separators are preserved *verbatim and in place*, so two configured
/// names that differ only in punctuation stay distinct rather than folding onto
/// one label. Any other character folded to `-`, and any truncation, is reported
/// through `folded`: that flag tells the caller the readable label no longer
/// carries the whole configured identity, so a digest must close the alias.
fn sanitize_label(value: &str, maximum_bytes: usize) -> FoldedIdentifier {
    let source = value.trim().to_lowercase();
    let mut replaced = false;
    let mut sanitized = String::with_capacity(source.len());
    let mut previous_was_hyphen = false;
    for character in source.chars() {
        // Collapse only a *run* of hyphens: the first one is never touching an
        // earlier hyphen, because there is none before it. That keeps `server.a`
        // (folded to `server-a`) distinct from `server-a` (never folded), while
        // still folding `server..a` to one separator.
        let character = if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            character
        } else {
            replaced = true;
            '-'
        };
        if character != '-' || !previous_was_hyphen {
            sanitized.push(character);
        }
        previous_was_hyphen = character == '-';
    }
    let truncated = sanitized.len() > maximum_bytes;
    FoldedIdentifier {
        value: prefix_bytes(&sanitized, maximum_bytes),
        folded: replaced || truncated,
    }
}

/// Fold a tool function name into the portable grammar. MCP function names are
/// already identifiers; anything else is folded to `_` and reported as folded.
fn sanitize_identifier(value: &str) -> FoldedIdentifier {
    let mut replaced = false;
    let mut sanitized = String::with_capacity(value.len());
    let mut previous_was_underscore = false;
    for byte in value.bytes() {
        let byte = if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
            byte
        } else {
            replaced = true;
            b'_'
        };
        if byte != b'_' || !previous_was_underscore {
            sanitized.push(char::from(byte));
        }
        previous_was_underscore = byte == b'_';
    }
    FoldedIdentifier {
        value: if sanitized.is_empty() {
            "tool".to_owned()
        } else {
            sanitized
        },
        folded: replaced,
    }
}

/// The longest whole-character prefix of `value` that fits `maximum_bytes`.
/// Slicing on a character boundary keeps the result valid UTF-8 even when a
/// folded identifier still contains multi-byte characters.
fn prefix_bytes(value: &str, maximum_bytes: usize) -> String {
    let mut end = value.len().min(maximum_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// 128-bit digest of the exact `(server, tool)` identity before folding or
/// truncation, so punctuation, long shared prefixes, and separate servers stay
/// distinct.
fn identity_digest(server_id: &str, tool: &str) -> String {
    let digest = Sha256::digest(format!("mcp://{server_id}/{tool}").as_bytes());
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn adashi_names_with_generated_server_ids_fit_the_provider_contract() {
        let server = "mcp.166dddff4b6840dba8aed1edbbb9e427";
        for tool in [
            "adashi_design",
            "adashi_get_rule_injections",
            "adashi_design_set_element_descriptions",
            "adashi_mockup_list_pending_revisions",
            "adashi_append_memory_note",
        ] {
            let name = mcp_provider_name(server, "Adashi", tool);
            assert_eq!(name, tool, "a self-naming function keeps its own name");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            );
            assert_eq!(name, mcp_provider_name(server, "Adashi", tool));
        }
    }

    /// The whole point of the change: the model sees the name it must call, with
    /// no opaque digest and no repeated server label.
    #[test]
    fn configured_server_names_produce_readable_aliases() {
        let server = "mcp.166dddff4b6840dba8aed1edbbb9e427";
        assert_eq!(
            mcp_provider_name(server, "Adashi", "adashi_design"),
            "adashi_design"
        );
        assert_eq!(
            mcp_provider_name(server, "Adashi", "adashi_get_rule_injections"),
            "adashi_get_rule_injections"
        );
        // A function that does not name its own server is qualified by it once.
        assert_eq!(
            mcp_provider_name(server, "Adashi", "get_memory"),
            "adashi_get_memory"
        );
        // Without a configured name the server's own id is the qualifier.
        assert_eq!(
            mcp_provider_name(server, "", "get_memory"),
            "166dddff4b6840dba8aed1ed_get_memory"
        );
        for name in [
            mcp_provider_name(server, "Adashi", "adashi_design"),
            mcp_provider_name(server, "", "get_memory"),
        ] {
            assert!(!name.starts_with("mcp__"), "{name}");
            assert!(!name.contains("adashi_adashi"), "{name}");
        }
    }

    /// An alias encodes the server label and the function name, which is all the
    /// model needs. The exact identity — including which server contributed it —
    /// lives in `capability_id`, and dispatch always requires both to match, so a
    /// collision can never route a call to the wrong server. These are the pairs
    /// that legitimately share an alias, recorded here so the behaviour is
    /// deliberate rather than incidental.
    #[test]
    fn an_alias_is_ambiguous_only_between_servers_whose_labels_are_identical() {
        let same_label = mcp_provider_name("server", "Adashi", "read");
        for server_id in ["server.a", "server_a", "server__a", "server-a"] {
            assert_eq!(mcp_provider_name(server_id, "Adashi", "read"), same_label);
        }
        // Distinct functions on one server never share an alias, which is the
        // ambiguity the model would actually experience.
        let distinct: BTreeSet<_> = [
            "adashi_design",
            "adashi_get_rule_injections",
            "adashi_memory",
            "read",
            "read_a",
            "read.a",
            "a__read",
            "读取",
        ]
        .iter()
        .map(|tool| mcp_provider_name("server", "Adashi", tool))
        .collect();
        assert_eq!(distinct.len(), 8);
    }

    /// Folding and truncation still cannot merge two functions: whenever the
    /// readable form gives up part of the identity, the digest restores it.
    #[test]
    fn names_do_not_alias_after_folding_truncating_or_joining_identifiers() {
        let long = "adashi_design_set_element_descriptions_and_review_the_result";
        let padding = "Another Server Label That Is Far Too Long To Fit Here";
        let label = server_label("unused", padding);
        assert_eq!(label.qualified, "another-server-label-tha");
        assert_eq!(label.unqualified, padding.to_lowercase().replace(' ', "-"));
        assert!(label.folded, "a truncated label must announce that it folded");
        assert_eq!(long.len(), 60, "the fixture must exceed the tool budget");
        // "adashi_design_set_element_descriptions" is a real Adashi function name
        // and a strict prefix of `long`, so the pair also pins the boundary where
        // a name stops fitting and starts being truncated.
        let cases = [
            ("server", "Adashi", "read.a"),
            ("server", "Adashi", "read_a"),
            ("server", "Adashi", "a__read"),
            ("server", "Adashi", "ab"),
            ("server", "Adashi", "adashi_design_set_element_descriptions"),
            ("server", "Adashi", "adashi_design_set_element_something_else"),
            ("server", "Adashi", "读取"),
            ("server", "Adashi", long),
            ("server", "Adashi", &format!("{}x", &long[..long.len() - 1])),
            ("server", "Another Server Label That Is Far Too Long", "adashi_memory"),
        ];
        let names: BTreeSet<_> = cases
            .iter()
            .map(|(server, label, tool)| mcp_provider_name(server, label, tool))
            .collect();
        assert_eq!(names.len(), cases.len());
        // The function's own name wins for its own server, and a label that had to
        // be truncated still closes the alias with its digest.
        assert_eq!(
            mcp_provider_name("server", padding, "adashi_design"),
            "another-server-label-tha_adashi_design_bec7992a"
        );
        assert_eq!(
            mcp_provider_name("server", "Adashi", "adashi_design"),
            "adashi_design"
        );
        assert!(names.iter().all(|name| {
            name.len() <= 64
                && name.is_ascii()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        }));
    }

    /// The one identity an alias genuinely cannot separate: two labels that fold
    /// and truncate to the same readable text, carrying a function of the same
    /// name on servers whose ids differ. Nothing survives the fold to key a
    /// digest on, so the alias is shared. Dispatch is unaffected — it requires
    /// `capability_id` *and* name to match — and the fix is a distinct display
    /// name in Settings. This is asserted so the boundary stays deliberate.
    #[test]
    fn a_name_the_alias_cannot_carry_is_documented_rather_than_hidden() {
        let long = "adashi_design_set_element_descriptions_and_review_the_result";
        let shared = "Another Server Label That Is Far Too Long To Fit Here";
        assert_eq!(
            mcp_provider_name("server", shared, long),
            mcp_provider_name("server", &format!("{shared}!"), long)
        );
    }

    /// Long server labels and long function names still cannot collide, because
    /// the reserved digest suffix covers the identity the fold discarded.
    #[test]
    fn over_long_identifiers_keep_a_digest_suffix_instead_of_folding() {
        let label = "An Extremely Long Configured MCP Server Display Name";
        let first = mcp_provider_name("mcp.one", label, &"a".repeat(80));
        let second = mcp_provider_name("mcp.one", label, &format!("{}b", "a".repeat(79)));
        assert!(first.len() <= 64, "{first}");
        assert!(second.len() <= 64, "{second}");
        assert_ne!(first, second);
        assert!(first
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')));
        assert!(mcp_provider_name("mcp.one", label, "adashi_design").len() <= 64);
    }

    #[test]
    fn existing_usable_frozen_aliases_remain_exact() {
        assert_eq!(
            portable_frozen_name("serv.fixture", "echo", "mcp__serv_fixture__echo"),
            "mcp__serv_fixture__echo"
        );
        assert_eq!(
            portable_frozen_name("server", "tool", &"x".repeat(64)),
            "x".repeat(64)
        );
        assert_eq!(
            portable_frozen_name("server", "tool", &"x".repeat(65)),
            mcp_provider_name("server", "server", "tool")
        );
    }
}
