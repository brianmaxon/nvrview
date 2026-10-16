//! What the alert filters learned, through the core (FFI minor 17): the
//! insight layers of a camera, a repeat held back, the reset, and the scene
//! memory's file in the data folder. On the simulated clock.

use super::*;

fn insights(h: &Harness, q: Value) -> Value {
    serde_json::from_str(&h.core.alert_insights_get(&q.to_string()).unwrap()).unwrap()
}

#[test]
fn insight_layers_a_repeat_the_reset_and_the_saved_memory() {
    let mut h = Harness::new(1, NOON_S, true);
    assert_eq!(
        h.core.alert_insights_get("{}"),
        Err(AlertError::NotConfigured)
    );
    h.configure(
        json!({"cameras": [{"camera": h.cam(0), "exposure": "outside"}],
                       "acquisition": {"recheck": false}}),
    );
    assert_eq!(insights(&h, json!({}))["cameras"], json!([]));
    assert_eq!(
        h.core.alert_insights_get(r#"{"camera":{"channel":0}}"#),
        Err(AlertError::InvalidArgument)
    );

    let jpeg = b"\xff\xd8example-frame\xff\xd9";
    let put = |h: &Harness| {
        let id = h
            .core
            .alerts_trigger_put(
                &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
            )
            .unwrap();
        h.core
            .alert_evidence_put(
                &json!({"trigger_id": id, "frame": 0, "doc": {}}).to_string(),
                jpeg,
            )
            .unwrap();
        id
    };
    let first = put(&h);
    h.run_until_with("a decision", 20, &mut |_, _| Answer::Person, |h| {
        h.decisions().len() == 1
    });
    assert_eq!(h.decisions()[0]["decision"], "alert");

    // The camera now has a person memory and a last alerted set.
    let list = insights(&h, json!({}));
    assert_eq!(list["cameras"][0]["camera"], h.cam(0), "{list}");
    assert_eq!(list["cameras"][0]["classes"], json!(["person"]));
    let one = insights(&h, json!({"camera": h.cam(0)}));
    let ids: Vec<&str> = one["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["scene.person", "repeat.last", "thresholds"], "{one}");
    let grid = &one["layers"][0];
    assert_eq!(grid["kind"], "grid");
    let values = grid["values"].as_array().unwrap();
    assert_eq!(values.len(), 64 * 36);
    assert!(values.iter().any(|v| v.as_f64().unwrap() > 0.0));
    assert_eq!(one["layers"][1]["kind"], "boxes");
    assert_eq!(one["layers"][1]["boxes"][0]["class"], "person");
    assert_eq!(one["layers"][2]["kind"], "scalar");
    assert_eq!(one["night"], false);
    assert_eq!(one["picture"]["trigger_id"], first, "{one}");
    assert_eq!(one["picture"]["frame"], 0);

    // The same person in the same place a minute later: a repeat.
    h.sim.advance(Duration::from_secs(60));
    put(&h);
    h.run_until_with("a second decision", 20, &mut |_, _| Answer::Person, |h| {
        h.decisions().len() == 2
    });
    let d = &h.decisions()[1];
    assert_eq!(d["decision"], "quiet", "{d}");
    assert_eq!(d["reason"], "repeat", "{d}");
    assert!(
        d["explanation"]
            .as_str()
            .unwrap()
            .ends_with("Not alerted: the same detections as the alert 1 min ago (repeat)."),
        "{d}"
    );

    // The memory was saved to the data folder.
    let file = h
        ._dir
        .as_ref()
        .unwrap()
        .path()
        .join("alerts/scene-memory.bin");
    h.run_until("saved", 5, |_| file.exists());
    let saved = alerts::filters::SceneMemory::from_bytes(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(saved.len(), 1);

    // Reset: forgotten, and saved at once.
    h.core
        .alert_insights_reset(&json!({"camera": h.cam(0)}).to_string())
        .unwrap();
    assert_eq!(insights(&h, json!({}))["cameras"], json!([]));
    let one = insights(&h, json!({"camera": h.cam(0)}));
    assert_eq!(one["layers"].as_array().unwrap().len(), 1, "{one}");
    let saved = alerts::filters::SceneMemory::from_bytes(&std::fs::read(&file).unwrap()).unwrap();
    assert!(saved.is_empty());
    assert_eq!(
        h.core.alert_insights_reset("{}"),
        Err(AlertError::InvalidArgument)
    );
}
