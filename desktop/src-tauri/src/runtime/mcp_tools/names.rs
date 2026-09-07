//! Provider aliases are presentation identifiers, never MCP dispatch authority.

use sha2::{Digest, Sha256};

/// Keep a readable tool prefix within the portable 64-byte provider limit. The
/// 128-bit digest covers both exact identifiers before folding or truncation,
/// so punctuation, long shared prefixes, and separate servers stay distinct.
pub(crate) fn mcp_provider_name(server_id: &str, tool: &str) -> String {
    let digest = Sha256::digest(format!("mcp://{server_id}/{tool}").as_bytes());
    let suffix: String = tool
        .bytes()
        .take(25)
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect();
    let hash = format!("{digest:x}");
    format!("mcp__{}__{suffix}", &hash[..32])
}

/// Preserve aliases that may already appear in frozen provider exchanges.
/// Older overlong aliases could never pass request validation; repair only that
/// wire projection without changing the persisted definition or capability.
pub(crate) fn portable_frozen_name(server_id: &str, tool: &str, frozen: &str) -> String {
    if !frozen.is_empty()
        && frozen.len() <= 64
        && frozen
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        frozen.to_owned()
    } else {
        mcp_provider_name(server_id, tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn adashi_names_with_generated_server_ids_fit_the_provider_contract() {
        let server = "mcp.166dddff4b6840dba8aed1edbbb9e427";
        for tool in [
            "adashi_get_rule_injections",
            "adashi_design_set_element_descriptions",
            "adashi_mockup_list_pending_revisions",
            "adashi_append_memory_note",
        ] {
            let name = mcp_provider_name(server, tool);
            assert!(name.len() <= 64, "{name}");
            assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
            assert_eq!(name, mcp_provider_name(server, tool));
        }
    }

    #[test]
    fn names_do_not_alias_after_folding_truncating_or_joining_identifiers() {
        let pairs = [
            ("server.a", "read"),
            ("server_a", "read"),
            ("server", "read.a"),
            ("server", "read_a"),
            ("server__a", "read"),
            ("server", "a__read"),
            ("server", "adashi_design_set_element_descriptions"),
            ("server", "adashi_design_set_element_something_else"),
            ("server", "读取"),
        ];
        let names: BTreeSet<_> = pairs.iter().map(|(s, t)| mcp_provider_name(s, t)).collect();
        assert_eq!(names.len(), pairs.len());
        assert!(names.iter().all(|n| n.len() <= 64 && n.is_ascii()));
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
            mcp_provider_name("server", "tool")
        );
    }
}
