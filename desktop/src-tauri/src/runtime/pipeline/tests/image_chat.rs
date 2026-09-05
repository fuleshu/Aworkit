//! Image bytes cross only the last provider boundary, leaving the frozen
//! authority, checkpoints, and durable replay under their existing size bounds.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

#[test]
fn image_larger_than_history_limit_is_materialized_after_authority_and_replayed() {
    let root = TempDir::new().unwrap();
    let (mut pipeline, credentials, metadata, calls, saw_secret) =
        setup(&root, ScriptedBehavior::Succeed);
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let factory = Arc::new(ScriptedProviderFactory {
        calls: calls.clone(),
        behavior: ScriptedBehavior::Succeed,
        saw_secret,
        observed_inputs: Some(observed.clone()),
    });
    pipeline.provider_factory = factory.clone();
    let mut random = 42_u32;
    let bitmap = image::RgbImage::from_fn(640, 640, |_, _| {
        let mut rgb = [0; 3];
        for byte in &mut rgb {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            *byte = (random >> 24) as u8;
        }
        image::Rgb(rgb)
    });
    let mut encoded = std::io::Cursor::new(Vec::new());
    bitmap
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let bytes = encoded.into_inner();
    assert!(bytes.len() > 1024 * 1024);
    let store = crate::runtime::images::ChatImageStore::new(root.path());
    let image = store
        .import("large.png".into(), STANDARD.encode(&bytes))
        .unwrap();
    let thumbnail = store.thumbnail(&image).unwrap();
    let thumbnail_bytes = STANDARD
        .decode(thumbnail.split_once(',').unwrap().1)
        .unwrap();
    let thumbnail_image = image::load_from_memory(&thumbnail_bytes).unwrap();
    assert!(thumbnail_image.width() <= 256 && thumbnail_image.height() <= 192);
    assert!(thumbnail_bytes.len() < bytes.len());
    let mut execution = request(metadata);
    execution.messages[0].content.clear();
    execution.messages[0].images = vec![image.clone()];
    let result = pipeline.execute(execution.clone()).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let inputs = observed.lock().unwrap();
    let images = inputs[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|message| message.get("images"))
        .unwrap();
    assert_eq!(images[0]["data"], STANDARD.encode(&bytes));
    drop(inputs);
    let record = pipeline
        .records
        .execution(&execution.request_id)
        .unwrap()
        .unwrap();
    let serialized = serde_json::to_string(&record).unwrap();
    assert!(serialized.contains(&image.id));
    assert!(serialized.len() < 128 * 1024);
    drop(pipeline);
    let pipeline = WorkflowExecutionPipeline::compose(root.path(), credentials, factory).unwrap();
    assert!(pipeline.execute(execution).unwrap().replayed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
