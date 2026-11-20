use super::*;

fn frames() -> Vec<Value> {
    sequence()
        .into_iter()
        .enumerate()
        .map(|(seq, state)| json!({"seq":seq,"state":state}))
        .collect()
}

#[test]
fn m6_partial_measurements_do_not_invent_return_and_validate_native_state_types() {
    let mut world = super::super::world::World::default();
    for count in 0..=8 {
        world.ui_states = frames().into_iter().take(count).collect();
        let measured = measurement(&world);
        assert_eq!(measured["ui_states"], json!(world.ui_states));
        assert_eq!(measured["frame_count"], if count >= 5 { 2 } else { 0 });
        assert_eq!(
            measured["return_state"],
            if count >= 6 {
                json!({"state":"thinking"})
            } else {
                json!({})
            }
        );
        assert_eq!(validate_measurement(&measured), Ok(()));
    }
    let original = measurement(&world);
    for invalid in [
        json!({}),
        json!({"state":"future"}),
        json!({"state":"emotion","frame_ms":80}),
    ] {
        let mut measured = original.clone();
        measured["ui_states"][0]["state"] = invalid.clone();
        assert_eq!(
            validate_measurement(&measured),
            Err("m6: malformed measured screen frame".into())
        );
        measured = original.clone();
        measured["return_state"] = invalid.clone();
        if invalid != json!({}) {
            assert_eq!(
                validate_measurement(&measured),
                Err("m6: malformed measured return state".into())
            );
        }
    }
}

#[test]
fn m6_ordered_oracle_requires_exact_values_and_increasing_native_sequences() {
    let expected = json!({"sequence":sequence()});
    assert_eq!(ordered(&frames(), &expected), Ok(()));
    for (field, value) in [
        ("name", json!("sad")),
        ("frame_ms", json!(81)),
        ("frames", json!(["a", "b"])),
    ] {
        let mut wrong = frames();
        wrong[4]["state"][field] = value;
        let message = ordered(&wrong, &expected).unwrap_err();
        assert_eq!(
            message,
            format!("m6: expected state {}; observed {}", state(), wrong[4]["state"])
        );
    }
    for index in 1..8 {
        let mut wrong = frames();
        wrong[index]["seq"] = wrong[index - 1]["seq"].clone();
        assert_eq!(
            ordered(&wrong, &expected),
            Err("m6: screen sequence must strictly increase".into())
        );
    }
    let mut swapped = frames();
    swapped.swap(5, 6);
    assert!(
        ordered(&swapped, &expected)
            .unwrap_err()
            .starts_with("m6: expected state")
    );
    let mut missing = frames();
    missing.remove(4);
    assert_eq!(
        ordered(&missing, &expected),
        Err("m6: ordered screen state count mismatch".into())
    );
}

#[test]
fn m6_still_empty_duplicate_and_malformed_frames_fail_before_value_comparison() {
    for (frames_value, distinct) in [(json!([]), 0), (json!(["(^_^)"]), 1), (json!(["same", "same"]), 1)] {
        let mut actual = frames();
        actual[4]["state"]["frames"] = frames_value;
        assert_eq!(
            ordered(&actual, &json!({"sequence":sequence()})),
            Err(format!(
                "m6: animation frame count {distinct}; expected at least 2 distinct frames"
            ))
        );
    }
    for invalid in [
        json!({}),
        json!({"seq":-1,"state":state()}),
        json!({"seq":1,"state":{"state":"unknown"}}),
    ] {
        assert_eq!(
            frame(&invalid, None, &state()),
            Err("m6: malformed screen frame".into())
        );
    }
    assert_eq!(ordered(&[], &json!({})), Err("m6: missing expected sequence".into()));
}
