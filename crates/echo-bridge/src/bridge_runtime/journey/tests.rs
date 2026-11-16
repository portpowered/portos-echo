use super::*;

mod bridge_spine_tests {
    use super::*;

    #[test]
    fn bridge_spine_only_explicit_completion_closes_audio() {
        let mut source = Source::new().unwrap();
        for value in [
            json!({"type":"future.event"}),
            json!({"type":"response.done"}),
            json!({}),
        ] {
            source.event(LiveEvent::Other(value), false).unwrap();
            assert!(!source.audio_done);
        }
        source
            .event(LiveEvent::Other(json!({"type":"response.audio.done"})), false)
            .unwrap();
        assert!(source.audio_done);
        assert_eq!(source.queue.len(), 0);
        assert_eq!(source.reply.finish().unwrap(), Vec::<u8>::new());
        assert_eq!(
            source.event(
                LiveEvent::Error {
                    extra: std::collections::BTreeMap::default()
                },
                false
            ),
            Err(Error::Unavailable)
        );
    }

    #[test]
    fn bridge_spine_echo_acknowledgment_is_lab_only_and_validated() {
        let mut source = Source::new().unwrap();
        let event = LiveEvent::Other(json!({"type":"fake.audio.frames","count":3}));
        source.event(event.clone(), false).unwrap();
        assert_eq!(source.acknowledged, 0);
        source.event(event, true).unwrap();
        assert_eq!(source.acknowledged, 3);
        for count in [json!(-1), json!("3"), json!(null)] {
            assert_eq!(
                source.event(
                    LiveEvent::Other(json!({"type":"fake.audio.frames","count":count})),
                    true
                ),
                Err(Error::Decode)
            );
        }
        assert!(!source.audio_done);
    }

    #[test]
    fn bridge_spine_reply_queue_keeps_newest_complete_frames_and_tail() {
        let mut source = Source::new().unwrap();
        for value in 0..130_u8 {
            source.enqueue(&vec![value; 960]);
        }
        source.enqueue(&[255; 13]);
        assert_eq!(source.partial, vec![255; 13]);
        assert_eq!(source.queue.drop_count(), 2);
        for value in 2..130_u8 {
            assert_eq!(source.queue.pop(), Some(vec![value; 960]));
        }
        assert_eq!(source.queue.pop(), None);
    }
}

mod bridge_pacing_cut_tests {
    use super::*;

    #[test]
    fn bridge_pacing_cut_excludes_prior_evictions_and_partial_tail() {
        let mut source = Source::new().unwrap();
        for value in 0..130_u8 {
            source.enqueue(&vec![value; 960]);
        }
        source.enqueue(&[255; 13]);
        assert_eq!(source.framed, 130);
        assert_eq!(source.queue.drop_count(), 2);
        assert_eq!(source.discard_reply(), 128);
        assert_eq!(source.queue.drop_count(), 0); // FrameQueue::clear resets its interval statistics.
        assert_eq!(source.queue.pop(), None);
        assert_eq!(source.partial, Vec::<u8>::new());
        assert_eq!(source.discard_reply(), 0);
    }
}
