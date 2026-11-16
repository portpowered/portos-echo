use std::collections::BTreeMap;

#[derive(Default)]
struct Gesture {
    active: bool,
    x: Option<i64>,
    y: Option<i64>,
}

#[derive(Debug, PartialEq)]
pub(super) struct Point {
    pub node: String,
    pub x: i64,
    pub y: i64,
}

#[derive(Default)]
pub(super) struct Decoder {
    gestures: BTreeMap<String, Gesture>,
}

impl Decoder {
    pub fn line(&mut self, line: &str) -> Result<Option<Point>, String> {
        let Some((_, event)) = line.split_once(']') else {
            return Ok(None);
        };
        let fields: Vec<_> = event.split_whitespace().collect();
        let [node, kind, code, value] = fields.as_slice() else {
            return Ok(None);
        };
        let node = node.trim_end_matches(':');
        let gesture = self.gestures.entry(node.to_owned()).or_default();
        match (*kind, *code, *value) {
            ("EV_KEY", "BTN_TOUCH", "UP") | ("EV_ABS", "ABS_MT_TRACKING_ID", "ffffffff") => {
                if gesture.active {
                    gesture.active = false;
                    return Ok(Some(Point {
                        node: node.to_owned(),
                        x: gesture.x.ok_or("calibrate: tap has no position")?,
                        y: gesture.y.ok_or("calibrate: tap has no position")?,
                    }));
                }
            }
            ("EV_KEY", "BTN_TOUCH", "DOWN") | ("EV_ABS", "ABS_MT_TRACKING_ID", _) => begin(gesture),
            ("EV_ABS", "ABS_MT_POSITION_X" | "ABS_MT_POSITION_Y", _) if gesture.active => {
                let coordinate = i64::from_str_radix(value, 16).map_err(|_| "calibrate: malformed coordinate")?;
                if *code == "ABS_MT_POSITION_X" {
                    gesture.x = Some(coordinate);
                } else {
                    gesture.y = Some(coordinate);
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

fn begin(gesture: &mut Gesture) {
    // Android can emit both tracking-id and BTN_TOUCH for one gesture.
    if !gesture.active {
        *gesture = Gesture {
            active: true,
            x: None,
            y: None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn feed(decoder: &mut Decoder, node: &str, events: &[&str]) -> Result<Vec<Point>, String> {
        events.iter().try_fold(Vec::new(), |mut points, event| {
            if let Some(point) = decoder.line(&format!("[ 1.000] {node}: {event}"))? {
                points.push(point);
            }
            Ok(points)
        })
    }

    #[test]
    fn interleaving_last_position_and_duplicate_release() {
        let mut decoder = Decoder::default();
        let a = "/dev/input/event4";
        let b = "/dev/input/event5";
        assert_eq!(
            feed(
                &mut decoder,
                a,
                &["EV_KEY BTN_TOUCH DOWN", "EV_ABS ABS_MT_POSITION_X 00000001"]
            )
            .unwrap(),
            vec![]
        );
        assert_eq!(
            feed(
                &mut decoder,
                b,
                &[
                    "EV_ABS ABS_MT_TRACKING_ID 00000004",
                    "EV_ABS ABS_MT_POSITION_X 0000000a",
                    "EV_ABS ABS_MT_POSITION_Y 00000014",
                    "EV_ABS ABS_MT_TRACKING_ID ffffffff"
                ]
            )
            .unwrap(),
            vec![Point {
                node: b.into(),
                x: 10,
                y: 20
            }]
        );
        assert_eq!(
            feed(
                &mut decoder,
                a,
                &[
                    "EV_ABS ABS_MT_TRACKING_ID 00000001",
                    "EV_ABS ABS_MT_POSITION_X 00000040",
                    "EV_ABS ABS_MT_POSITION_Y 00000040",
                    "EV_KEY BTN_TOUCH UP",
                    "EV_ABS ABS_MT_TRACKING_ID ffffffff"
                ]
            )
            .unwrap(),
            vec![Point {
                node: a.into(),
                x: 64,
                y: 64
            }]
        );
        assert_eq!(
            feed(&mut decoder, a, &["EV_KEY BTN_TOUCH DOWN", "EV_KEY BTN_TOUCH UP"]).unwrap_err(),
            "calibrate: tap has no position"
        );
    }

    #[test]
    fn five_points_in_order_for_either_release_encoding() {
        let expected = [(64, 64), (1216, 64), (1216, 736), (64, 736), (640, 400)];
        for tracking in [false, true] {
            let mut decoder = Decoder::default();
            let mut points = Vec::new();
            for (x, y) in expected {
                let start = if tracking {
                    "EV_ABS ABS_MT_TRACKING_ID 00000001"
                } else {
                    "EV_KEY BTN_TOUCH DOWN"
                };
                let end = if tracking {
                    "EV_ABS ABS_MT_TRACKING_ID ffffffff"
                } else {
                    "EV_KEY BTN_TOUCH UP"
                };
                points.extend(
                    feed(
                        &mut decoder,
                        "/dev/input/event4",
                        &[
                            start,
                            &format!("EV_ABS ABS_MT_POSITION_X {x:08x}"),
                            &format!("EV_ABS ABS_MT_POSITION_Y {y:08x}"),
                            end,
                        ],
                    )
                    .unwrap(),
                );
            }
            assert_eq!(points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(), expected);
            writeln!(std::io::stderr(), "decoded points: {points:?}").unwrap();
        }
    }
}
