use super::*;
use serde_json::json;

fn adaptive() -> Policy {
    Policy {
        mode: Mode::Adaptive,
        minimum_bytes: 256,
        minimum_savings: 0.05,
        ..Default::default()
    }
}
const REF: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn tables_roundtrip_sparse_null_nested_constants_and_hostile_keys() {
    for seed in 0..40 {
        let rows:Vec<_>=(0..80).map(|i|{
            let mut row=json!({"id":i,"enabled":i%2==0,"number":1.25,"unicode":"日本語🦀","string":"{\"x\":1}","~slash/key":i+seed,
                "nested":{"entries":(0..8).map(|j|json!({"name":"long column name value","index":j})).collect::<Vec<_>>()}});
            if i%3==0 {row["nullable"]=Value::Null;}
            row
        }).collect();
        let value = json!({"format":"aworkit.table.v1","rows":rows});
        let compressed = lossless::table(&value).unwrap();
        assert_eq!(lossless::unpack(&compressed), Some(value));
    }
}

#[test]
fn log_templates_preserve_every_byte_and_variable() {
    let text = (0..120)
        .map(|i| {
            format!(
                "2026-09-08T00:00:{i:02}Z\tINFO worker-{i} processed job {} in region 日本\r\n",
                i * 99
            )
        })
        .collect::<String>();
    let value = lossless::templates(&text).unwrap();
    assert_eq!(lossless::expand_templates(&value).unwrap(), text);
    assert!(value.to_string().len() < text.len());
}

#[test]
fn gates_include_metadata_and_tokenizer_and_never_expand() {
    let value=json!((0..100).map(|i|json!({"long_repeated_column_name":i,"another_repeated_column_name":"constant","message":"retained"})).collect::<Vec<_>>());
    for tokenizer in [Tokenizer::Estimate, Tokenizer::Cl100k, Tokenizer::O200k] {
        let policy = Policy {
            tokenizer,
            ..Default::default()
        };
        let result = compress(&value, "", "", &policy, Some(REF), 65536).unwrap();
        assert!(result.metrics.after_tokens < result.metrics.before_tokens);
        assert!(result.metrics.after_bytes < result.metrics.before_bytes);
        assert!(!result.metrics.lossy);
        assert!(compress(&value, "", "", &policy, Some(REF), 256).is_none());
    }
    assert!(compress(&json!("tiny"), "", "", &adaptive(), Some(REF), 65536).is_none());
}

#[test]
fn statistical_selection_keeps_errors_rare_fields_categories_and_change_points() {
    let mut rows:Vec<_>=(0..120).map(|i|json!({"i":i,"latency":20,"status":"ok","description":format!("row {i} {}","ordinary observation ".repeat(12))})).collect();
    rows[59]["latency"] = json!(9000);
    rows[65]["rare"] = json!("audit fact");
    rows[73]["status"] = json!("unusual");
    rows[89]["description"] = json!("fatal: unique failure");
    let value = extract::rows(&json!(rows), "latency", &adaptive()).unwrap();
    let indices: Vec<_> = value["selected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["index"].as_u64().unwrap())
        .collect();
    for i in [0, 58, 59, 60, 65, 73, 89, 119] {
        assert!(indices.contains(&i), "missing {i}: {indices:?}");
    }
    assert!(indices.len() < 120);
}

#[test]
fn mandatory_content_defeats_target_without_disappearing() {
    let rows = json!(
        (0..100)
            .map(|i| json!({"error":format!("failure {i}")}))
            .collect::<Vec<_>>()
    );
    assert!(extract::rows(&rows, "", &adaptive()).is_none());
    let text = (0..100)
        .map(|i| format!("PROTECTED critical record {i}\n"))
        .collect::<String>();
    assert!(extract::text(&text, "", &adaptive()).is_none());
}

#[test]
fn adaptive_requires_retrieval_and_preserves_complete_source_spans() {
    let text = (0..100)
        .map(|i| {
            format!(
                "Observation {i} records unique measurement {} for component number {i}.\n",
                i * 199
            )
        })
        .collect::<String>();
    assert!(compress(&json!(text), "component 73", "", &adaptive(), None, 65536).is_none());
    let extracted = extract::text(&text, "component 73", &adaptive()).unwrap();
    for span in extracted["spans"].as_array().unwrap() {
        assert_eq!(
            span["text"],
            &text[span["start"].as_u64().unwrap() as usize..span["end"].as_u64().unwrap() as usize]
        );
    }
}

#[test]
fn diff_preserves_all_changes_and_headers() {
    let text = format!(
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,100 +1,100 @@\n{}-old important call\n+new important call\n{}",
        " context with detail\n".repeat(50),
        " after context\n".repeat(50)
    );
    let value = extract::text(&text, "", &adaptive()).unwrap().to_string();
    for needle in [
        "diff --git",
        "@@ -1,100",
        "-old important call",
        "+new important call",
    ] {
        assert!(value.contains(needle));
    }
}

#[test]
fn malformed_code_is_not_sent_to_prose_extraction() {
    let source = format!(
        "fn incomplete( {{\n{}",
        "this is not valid code\n".repeat(200)
    );
    assert!(
        compress(
            &json!(source),
            "",
            "broken.rs",
            &adaptive(),
            Some(REF),
            65536
        )
        .is_none()
    );
    let unknown = format!(
        "def unknown\n{}end\n",
        (0..200)
            .map(|i| format!("  variable_{i} = {i}\n"))
            .collect::<String>()
    );
    assert!(
        compress(
            &json!(unknown),
            "",
            "source.rb",
            &adaptive(),
            Some(REF),
            65536
        )
        .is_none()
    );
}

#[test]
fn syntax_outlines_support_eight_languages_and_keep_target_body() {
    let fixtures = [
        (
            "a.rs",
            format!(
                "use std::fmt;\nfn unrelated() {{ {} }}\nfn target() {{ println!(\"keep\"); }}",
                "let _x = 1;\n".repeat(100)
            ),
        ),
        (
            "a.py",
            format!(
                "import sys\ndef unrelated():\n{}\ndef target():\n    return 'keep'\n",
                "    x = 1\n".repeat(100)
            ),
        ),
        (
            "a.js",
            format!(
                "function unrelated() {{ {} }}\nfunction target() {{ return 'keep'; }}",
                "let x = 1;\n".repeat(100)
            ),
        ),
        (
            "a.ts",
            format!(
                "function unrelated(): void {{ {} }}\nfunction target() {{ return 'keep'; }}",
                "let x = 1;\n".repeat(100)
            ),
        ),
        (
            "a.go",
            format!(
                "package main\nfunc unrelated() {{ {} }}\nfunc target() {{ println(\"keep\") }}",
                "x := 1\n_ = x\n".repeat(100)
            ),
        ),
        (
            "a.java",
            format!(
                "class A {{ void unrelated() {{ {} }} String target() {{ return \"keep\"; }} }}",
                "int x = 1;\n".repeat(100)
            ),
        ),
        (
            "a.c",
            format!(
                "void unrelated() {{ {} }}\nchar* target() {{ return \"keep\"; }}",
                "int x = 1;\n".repeat(100)
            ),
        ),
        (
            "a.cpp",
            format!(
                "void unrelated() {{ {} }}\nconst char* target() {{ return \"keep\"; }}",
                "int x = 1;\n".repeat(100)
            ),
        ),
    ];
    for (path, source) in fixtures {
        let outline = code::outline(&source, path, "target", &adaptive())
            .unwrap_or_else(|| panic!("missing outline: {path}"));
        let text = outline.to_string();
        assert!(text.contains("keep"), "{path}");
        assert!(text.contains("unrelated"));
        assert!(!text.contains("int x = 1"));
        for span in outline["spans"].as_array().unwrap() {
            assert_eq!(
                span["text"],
                &source[span["start"].as_u64().unwrap() as usize
                    ..span["end"].as_u64().unwrap() as usize]
            );
        }
    }
}

#[test]
fn rich_blocks_preserve_images_annotations_and_order() {
    let value = json!({"content":[{"type":"image","data":"original opaque","mimeType":"image/png"},{"type":"text","text":(0..200).map(|i|format!("INFO repeated worker processed item {i}\n")).collect::<String>(),"annotations":{"audience":["assistant"]}},{"type":"resource_link","uri":"https://example.com"}],"isError":false});
    let result = compress(&value, "", "", &Policy::default(), Some(REF), 65536).unwrap();
    assert_eq!(value["content"][0], result.content["content"][0]);
    assert_eq!(value["content"][2], result.content["content"][2]);
    assert_eq!(
        value["content"][1]["annotations"],
        result.content["content"][1]["annotations"]
    );
}

#[test]
fn nested_and_direct_rich_results_keep_their_provider_shape() {
    let blocks = json!([{"type":"text","text":(0..300).map(|i|format!("INFO repeated worker received observation {i}\n")).collect::<String>()},{"type":"image","data":"opaque image","mimeType":"image/png"},{"type":"resource","resource":{"text":"preserved"}},{"type":"reasoning","text":"opaque reasoning"}]);
    let nested = json!({"server":"mcp","result":{"content":blocks,"_meta":{"signature":"opaque"}}});
    let result = compress(&nested, "", "", &Policy::default(), Some(REF), 65536).unwrap();
    assert_eq!(result.content["result"]["content"][1], blocks[1]);
    assert_eq!(result.content["result"]["_meta"], nested["result"]["_meta"]);
    let direct = compress(&blocks, "", "", &Policy::default(), Some(REF), 65536).unwrap();
    assert!(direct.content.is_array());
    assert_eq!(direct.content[1], blocks[1]);
    assert_eq!(direct.content[3], blocks[3]);
    let standalone =
        json!({"stdout":blocks[0]["text"],"nested":{"type":"reasoning","text":blocks[0]["text"]}});
    let result = compress(&standalone, "", "", &Policy::default(), Some(REF), 65536).unwrap();
    assert_eq!(result.content["nested"], standalone["nested"]);
}

#[test]
fn search_centers_long_single_record_on_the_matching_evidence() {
    let value = json!(format!(
        "{} unique_tail_identifier {}",
        "ordinary ".repeat(5000),
        "ending ".repeat(2000)
    ));
    let request=retrieval::Request::parse(&json!({"operation":"search","reference":REF,"query":"ordinary unique_tail_identifier","limit":2048})).unwrap();
    let result = retrieval::retrieve(&value, &request, 2048).unwrap();
    assert!(
        result["matches"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unique_tail_identifier")
    );
    assert!(result.to_string().len() <= 2048);
}

#[test]
fn retrieval_pages_reconstruct_utf8_and_json_pointer() {
    let text = "😀日\n\t\"\\".repeat(1000);
    let value = json!({"data":text});
    let mut offset = 0;
    let mut actual = String::new();
    loop {
        let request=retrieval::Request::parse(&json!({"operation":"read","reference":REF,"pointer":"/data","offset":offset,"limit":1024})).unwrap();
        let page = retrieval::retrieve(&value, &request, 2048).unwrap();
        assert!(page.to_string().len() <= 1024);
        actual.push_str(page["text"].as_str().unwrap());
        let Some(next) = page["nextOffset"].as_u64() else {
            break;
        };
        assert!(next as usize > offset);
        offset = next as usize;
    }
    assert_eq!(actual, text);
    let invalid = retrieval::Request::parse(
        &json!({"operation":"read","reference":REF,"pointer":"/data","offset":1}),
    )
    .unwrap();
    assert!(retrieval::retrieve(&value, &invalid, 2048).is_err());
}

#[test]
fn search_returns_rare_identifier_and_multilingual_original_spans() {
    let value = json!("ordinary data\n東京 珍しい失敗 identifier_739\nordinary metrics\n");
    let request = retrieval::Request::parse(
        &json!({"operation":"search","reference":REF,"query":"identifier_739 東京"}),
    )
    .unwrap();
    let result = retrieval::retrieve(&value, &request, 4096).unwrap();
    assert!(
        result["matches"][0]["text"]
            .as_str()
            .unwrap()
            .contains("identifier_739")
    );
    assert_eq!(result["totalMatches"], 1);
}

#[test]
fn closed_policy_and_arguments_reject_invalid_values() {
    assert!(serde_json::from_value::<Policy>(json!({"unknown":true})).is_err());
    assert!(
        Policy {
            target_ratio: f64::NAN,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    for v in [
        json!({"operation":"read"}),
        json!({"operation":"stats","owner":"other"}),
        json!({"operation":"search","reference":REF}),
        json!({"operation":"read","reference":"guessed"}),
    ] {
        assert!(retrieval::Request::parse(&v).is_err());
    }
}
