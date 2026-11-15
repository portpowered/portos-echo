//! T2 normalized capture replay; original data section placement is not retained.
use echo_contracts::wyoming::{Decoder, Event, Header, WyomingEvent, encode};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path};

fn payload(row: &Value, length: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if length == 0 {
        return Ok(vec![]);
    }
    // scripts/capture-wyoming.py feeds silence and sends constant i16(1) replies.
    Ok(match row["payload_sha256"].as_str().ok_or("missing payload digest")? {
        "e5a00aa9991ac8a5ee3109844d84a55583bd20572ad3ffcd42792f3c36b183ad" => {
            assert_eq!(length, 2048);
            assert_eq!(
                row["payload_prefix_b64"],
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            );
            vec![0; length]
        }
        "50b6224697c94c1f6982e779bb7f418d88f91aba4a560a729979e64f51545e9b" => {
            assert_eq!(length, 4800);
            assert_eq!(
                row["payload_prefix_b64"],
                "AQABAAEAAQABAAEAAQABAAEAAQABAAEAAQABAAEAAQA="
            );
            [1, 0].repeat(length / 2)
        }
        "e61f41d57db208c5f92a35c4ce7198570924a3fc87eeba83441fceee5d6a2865" => {
            assert_eq!(length, 1600);
            assert_eq!(
                row["payload_prefix_b64"],
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            );
            vec![0; length]
        }
        digest => return Err(format!("unsupported capture payload: {digest}").into()),
    })
}

fn expected_tag(event: &WyomingEvent) -> &str {
    match event {
        WyomingEvent::Describe(_) => "describe",
        WyomingEvent::Info(_) => "info",
        WyomingEvent::RunSatellite(_) => "run-satellite",
        WyomingEvent::PauseSatellite(_) => "pause-satellite",
        WyomingEvent::RunPipeline(_) => "run-pipeline",
        WyomingEvent::AudioStart(_) => "audio-start",
        WyomingEvent::AudioChunk(_) => "audio-chunk",
        WyomingEvent::AudioStop(_) => "audio-stop",
        WyomingEvent::Played(_) => "played",
        WyomingEvent::Ping(_) => "ping",
        WyomingEvent::Pong(_) => "pong",
        _ => "unexpected",
    }
}

#[test]
fn fixture_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/wyoming");
    let mut paths = fs::read_dir(directory)?
        .map(|p| p.map(|p| p.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "jsonl"));
    paths.sort();
    let mut total = 0;
    let mut stdout = std::io::stdout().lock();
    for path in &paths {
        let mut count = 0;
        for line in fs::read_to_string(path)?.lines().filter(|line| !line.is_empty()) {
            let row: Value = serde_json::from_str(line)?;
            let mut header: Header = serde_json::from_value(row["header"].clone())?;
            let bytes = payload(&row, header.payload_length)?;
            // The normalized merged data is replayed inline, not claimed as original wire placement.
            header.data_length = 0;
            let event = Event {
                header,
                data: None,
                payload: bytes,
            };
            let mut decoder = Decoder::new();
            let frames = decoder.feed(&encode(&event))?;
            assert_eq!(frames, std::slice::from_ref(&event), "{}", path.display());
            decoder.finish()?;
            let typed = frames[0].typed()?;
            assert_eq!(expected_tag(&typed), event.header.r#type);
            let wire = serde_json::to_value(&typed)?;
            assert_eq!(wire, json!({"type":event.header.r#type,"data":event.header.data}));
            assert_eq!(serde_json::from_value::<WyomingEvent>(wire)?, typed);
            count += 1;
        }
        writeln!(stdout, "{}: lines={count}", path.file_name().unwrap().display())?;
        total += count;
    }
    assert_eq!((paths.len(), total), (9, 214));
    writeln!(
        stdout,
        "fixture_round_trip: files={} lines={total} typed={total} other=0",
        paths.len()
    )?;
    Ok(())
}

#[test]
fn projection_merges_external_over_inline_without_changing_raw() -> Result<(), Box<dyn std::error::Error>> {
    let header: Header = serde_json::from_value(json!({"type":"audio-start", "data": {
        "rate":16000, "width":2, "channels":1, "inline":true
    }}))
    .unwrap();
    let event = Event {
        header,
        data: Some(json!({"rate":24000,"external":true})),
        payload: vec![],
    };
    let WyomingEvent::AudioStart(format) = event.typed().unwrap() else {
        return Err("wrong arm".into());
    };
    assert_eq!((format.rate, format.width, format.channels), (24000, 2, 1));
    assert_eq!(format.extra["inline"], true);
    assert_eq!(format.extra["external"], true);
    assert_eq!(event.header.data.unwrap()["rate"], 16000);
    Ok(())
}
