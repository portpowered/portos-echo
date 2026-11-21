//! Lip sync at the real bridge boundary: what the bridge says it sent for a
//! reply (`reply.quality` `lips`) against what the paired stream carried.
use echo_screen_contract::mouth::MouthBatch;
use serde_json::{Value, json};

/// The openness from which the page opens the mouth at all.
const OPENS_AT: u8 = 10;

/// Check one case's lip sync and report its latency relative to the audio
/// hand-off: `publish_max_ms` is the longest a sample waited from its frame's
/// hand-off to being sent, `lead_ms` how far ahead of its sound each sample
/// was sent.
pub fn measurement(records: &[Value], batches: &[Value]) -> Result<Value, String> {
    let sum = |name: &str| {
        records
            .iter()
            .filter(|record| record["event"] == "reply.quality")
            .filter_map(|record| record["lips"][name].as_u64())
            .sum::<u64>()
    };
    let publish_max_ms = records
        .iter()
        .filter(|record| record["event"] == "reply.quality")
        .filter_map(|record| record["lips"]["publish_max_ms"].as_u64())
        .max()
        .ok_or("lips: the reply reported no lip sync")?;
    let batches: Vec<MouthBatch> = batches
        .iter()
        .map(|batch| serde_json::from_value(batch.clone()).map_err(|_| "lips: malformed mouth batch"))
        .collect::<Result<_, _>>()?;
    let samples: Vec<_> = batches
        .iter()
        .flat_map(|batch| batch.samples.iter().map(|sample| (batch.sent_ms, sample)))
        .collect();
    let leads: Vec<u64> = samples
        .iter()
        .map(|(sent, sample)| sample.at_ms.saturating_sub(*sent))
        .collect();
    let (Some(lead_min), Some(lead_max)) = (leads.iter().min(), leads.iter().max()) else {
        return Err("lips: the screen stream carried no mouth sample".into());
    };
    let open_max = samples.iter().map(|(_, sample)| sample.open).max().unwrap_or(0);
    let ordered = samples.windows(2).all(|pair| pair[0].1.at_ms < pair[1].1.at_ms);
    let delivered = sum("batches").saturating_sub(sum("dropped"));
    let measured = json!({"batches":sum("batches"),"samples":sum("samples"),"dropped":sum("dropped"),
        "late":sum("late"),"publish_max_ms":publish_max_ms,"received_batches":batches.len(),
        "received_samples":samples.len(),"lead_ms":{"min":lead_min,"max":lead_max},"open_max":open_max});
    // Every batch the bridge counts as taken reached the stream; each sample
    // was sent before its sound, in order; the mouth opened.
    if u64::try_from(batches.len()) != Ok(delivered)
        || sum("late") != 0
        || *lead_min == 0
        || !ordered
        || open_max < OPENS_AT
    {
        return Err(format!("lips: unexpected lip sync: {measured}"));
    }
    Ok(measured)
}

#[cfg(test)]
mod tests {
    use super::measurement;
    use serde_json::{Value, json};

    fn quality(batches: u64, dropped: u64, late: u64) -> Value {
        json!({"event":"reply.quality","lips":{"batches":batches,"samples":3,"dropped":dropped,
            "publish_max_ms":80,"lead_min_ms":120,"late":late}})
    }

    fn batch(sent: u64, samples: &[(u64, u8)]) -> Value {
        let samples: Vec<_> = samples
            .iter()
            .map(|(at, open)| json!({"at_ms":at,"open":open,"vowel":"none"}))
            .collect();
        json!({"sent_ms":sent,"samples":samples})
    }

    #[test]
    fn t1_lips_measurement_reports_latency_and_refuses_what_a_page_could_not_use() {
        let good = [batch(1180, &[(1300, 70), (1320, 90)]), batch(1200, &[(1340, 0)])];
        let measured = measurement(&[quality(2, 0, 0)], &good).unwrap();
        assert_eq!(
            measured,
            json!({"batches":2,"samples":3,"dropped":0,"late":0,"publish_max_ms":80,"received_batches":2,
                "received_samples":3,"lead_ms":{"min":120,"max":140},"open_max":90})
        );
        // A dropped batch is one the stream did not carry.
        assert!(measurement(&[quality(3, 1, 0)], &good).is_ok());
        for (records, batches) in [
            (vec![], good.to_vec()),
            (vec![quality(2, 0, 0)], vec![]),
            (vec![quality(3, 0, 0)], good.to_vec()),
            (vec![quality(2, 0, 1)], good.to_vec()),
            // Sent at its sound, out of order, never open, malformed.
            (vec![quality(1, 0, 0)], vec![batch(1300, &[(1300, 70), (1320, 0)])]),
            (vec![quality(1, 0, 0)], vec![batch(1180, &[(1320, 70), (1300, 0)])]),
            (vec![quality(1, 0, 0)], vec![batch(1180, &[(1300, 9), (1320, 0)])]),
            (vec![quality(1, 0, 0)], vec![json!({"sent_ms":1})]),
        ] {
            assert!(measurement(&records, &batches).is_err(), "{records:?} {batches:?}");
        }
    }
}
