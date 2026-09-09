use super::*;

#[test]
fn structured_preview_retains_metadata_and_fitting_rich_blocks_without_mutating_original() {
    let image = json!({"type":"image","data":"A".repeat(2300),"mimeType":"image/png"});
    let annotated =
        json!({"type":"text","text":"distinct explanation","annotations":{"audience":["user"]}});
    let original = json!({"result":{"isError":true,"_meta":{"receipt":"exact"},
        "content":[{"type":"text","text":"long body ".repeat(9000)},image,annotated],
        "structuredContent":{"body":"large ".repeat(9000),"tail":"complete"}}});
    let snapshot = original.clone();
    let projected = bounded_content(&original, 4096, None).unwrap();
    assert!(projected.to_string().len() <= 4096);
    assert_eq!(
        serde_json::from_str::<Value>(&projected.to_string()).unwrap(),
        projected
    );
    assert_eq!(projected["aworkitOutput"]["truncated"], true);
    assert_eq!(projected["preview"]["result"]["isError"], true);
    assert_eq!(
        projected["preview"]["result"]["_meta"],
        snapshot["result"]["_meta"]
    );
    assert_eq!(
        projected["preview"]["result"]["structuredContent"]["tail"],
        "complete"
    );
    let blocks = projected["preview"]["result"]["content"]
        .as_array()
        .unwrap();
    assert!(blocks.contains(&image));
    assert!(blocks.contains(&annotated));
    assert_eq!(original, snapshot);
}

#[test]
fn escaped_unicode_nested_arrays_and_large_keys_obey_the_encoded_limit() {
    for budget in [1024, 2048, 4096, 65536] {
        for text in ["é🦀\"\\\n\t", "ascii", "\u{0000}"] {
            let original = json!({"tasks":(0..100).map(|id|json!({"id":id,"description":text.repeat(400)})).collect::<Vec<_>>(),
                "x".repeat(10000):"huge key", "total":100});
            if let Some(value) = bounded_content(&original, budget, None) {
                assert!(
                    value.to_string().len() <= budget,
                    "{budget}: {}",
                    value.to_string().len()
                );
                assert_eq!(value["preview"]["total"], 100);
                assert_eq!(value["aworkitOutput"]["truncated"], true);
                if budget < 10000 {
                    assert!(!value.to_string().contains(&"x".repeat(10000)));
                }
            }
        }
    }
}

#[test]
fn fitting_results_are_unchanged_and_oversized_media_is_omitted_whole() {
    let original = json!({"result":{"content":[],"structuredContent":{"tasks":[]}}});
    assert!(bounded_content(&original, 1024, None).is_none());
    let image = json!({"type":"image","data":"A".repeat(9000),"mimeType":"image/png"});
    let projected = bounded_content(&image, 1024, None).unwrap();
    assert!(projected["preview"].is_null());
    assert!(!projected.to_string().contains("mimeType"));
}

#[test]
fn text_and_reference_overheads_fit_and_previous_projection_is_stable() {
    let original = json!("quoted \"🦀\"\n".repeat(10000));
    let reference = "a".repeat(64);
    let projected = bounded_content(&original, 1024, Some(&reference)).unwrap();
    assert_eq!(projected["aworkitContext"]["reference"], reference);
    assert_eq!(projected["aworkitContext"]["omitted"], true);
    assert!(
        projected["preview"]
            .as_str()
            .unwrap()
            .ends_with("[Aworkit: string truncated]")
    );
    assert!(projected.to_string().len() <= 1024);
    assert!(bounded_content(&projected, 1024, None).is_none());
}
