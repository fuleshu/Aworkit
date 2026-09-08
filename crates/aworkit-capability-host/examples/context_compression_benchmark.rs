//! Reproducible local fixture measurements; no network or model calls.
use aworkit_capability_host::context_compression::{
    Mode, Policy, Tokenizer, compress, count, render,
};
use serde_json::{Value, json};
fn main() {
    let rows=json!((0..500).map(|i|json!({"event_id":i,"service":"checkout","environment":"production","response_time_ms":if i==319{9000}else{20},"status":if i==319{"ERROR"}else{"OK"},"message":"request completed"})).collect::<Vec<_>>());
    let logs = json!(
        (0..800)
            .map(|i| format!(
                "2026-09-08T00:00:00Z INFO checkout completed request {i} elapsed {}ms\n",
                i % 17
            ))
            .collect::<String>()
    );
    let code = json!(
        (0..30)
            .map(|i| format!(
                "fn worker_{i}() {{\n{}\n}}\n",
                (0..25)
                    .map(|j| format!("    let value_{i}_{j} = {j};\n"))
                    .collect::<String>()
            ))
            .collect::<String>()
    );
    let prose=json!((0..120).map(|i|format!("Observation {i} records the measurement {} for component_{}. This sentence retains the original qualification.\n",i*391,i%11)).collect::<String>());
    let cases: Vec<(&str, Value, &str, &str)> = vec![
        ("JSON event table", rows, "", "response_time_ms"),
        ("Log templates", logs, "", "checkout"),
        ("Source exploration", code, "fixture.rs", "worker_17"),
        ("Retrieved prose", prose, "", "component_9"),
    ];
    let mut report = Vec::new();
    for mode in [Mode::Lossless, Mode::Adaptive] {
        for (label, value, path, query) in &cases {
            let policy = Policy {
                mode,
                tokenizer: Tokenizer::O200k,
                ..Default::default()
            };
            let original = render(value);
            let before = count(&original, policy.tokenizer);
            let mut timings = Vec::new();
            let mut last = None;
            for _ in 0..21 {
                let start = std::time::Instant::now();
                last = compress(value, query, path, &policy, Some(&"a".repeat(64)), 524288);
                timings.push(start.elapsed().as_micros());
            }
            timings.sort_unstable();
            let (after, bytes, lossy) =
                last.as_ref().map_or((before, original.len(), false), |r| {
                    (
                        r.metrics.after_tokens,
                        r.metrics.after_bytes,
                        r.metrics.lossy,
                    )
                });
            report.push(json!({"case":label,"mode":mode,"beforeBytes":original.len(),"afterBytes":bytes,"beforeTokens":before,"afterTokens":after,"savingsPercent":(before-after) as f64/before as f64*100.0,"lossy":lossy,"p50Micros":timings[10],"p95Micros":timings[19]}));
        }
    }
    println!("{}",serde_json::to_string_pretty(&json!({"profile":if cfg!(debug_assertions){"debug"}else{"release"},"tokenizer":"o200k_base","iterations":21,"scope":"synthetic local observations; no model quality or billing measurement","results":report})).unwrap());
}
