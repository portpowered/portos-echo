#[cfg(test)]
mod tests {
    use echo_audio::{FrameQueue, OpusDecoder, OpusEncoder, Pacer, PcmResampler};
    use echo_contracts::wyoming::AudioFormat;
    use std::{collections::BTreeMap, io::Write, num::NonZeroUsize, time::Duration};

    fn converter(input_rate: u32, output_rate: u32) -> PcmResampler {
        PcmResampler::new(
            &AudioFormat {
                rate: input_rate,
                width: 2,
                channels: 1,
                extra: BTreeMap::new(),
            },
            output_rate,
        )
        .unwrap()
    }

    // Caller-owned fixed block assembly: no whole-stream PCM accumulation.
    struct Capture {
        pcm: PcmResampler,
        encoder: OpusEncoder,
        frame: Vec<i16>,
        packets: FrameQueue<(usize, Vec<u8>)>,
        count: usize,
    }

    impl Capture {
        fn new(capacity: usize) -> Self {
            Self {
                pcm: converter(16000, 48000),
                encoder: OpusEncoder::new().unwrap(),
                frame: Vec::with_capacity(960),
                packets: FrameQueue::new(NonZeroUsize::new(capacity).unwrap()),
                count: 0,
            }
        }

        fn assemble(&mut self, samples: &[i16]) {
            for &sample in samples {
                self.frame.push(sample);
                if self.frame.len() == 960 {
                    let mut packet = [0; 1275];
                    let size = self.encoder.encode(&self.frame, &mut packet).unwrap();
                    self.packets.push((self.count, packet[..size].to_vec()));
                    self.count += 1;
                    self.frame.clear();
                }
            }
            assert!(self.frame.len() < 960);
        }

        fn capture(&mut self, input: &[i16]) {
            let mut scratch = vec![0; self.pcm.output_capacity()];
            for mut chunk in input.chunks(137) {
                while !chunk.is_empty() {
                    let progress = self.pcm.process(chunk, &mut scratch).unwrap();
                    assert!(progress.consumed > 0);
                    chunk = &chunk[progress.consumed..];
                    self.assemble(&scratch[..progress.written]);
                }
            }
            loop {
                let written = self.pcm.finish(&mut scratch).unwrap();
                if written == 0 {
                    break;
                }
                self.assemble(&scratch[..written]);
            }
            assert_eq!(self.pcm.finish(&mut scratch).unwrap(), 0);
        }

        fn reset(&mut self) {
            self.pcm.reset();
            self.encoder = OpusEncoder::new().unwrap();
            self.frame.clear();
            self.packets.clear();
            self.count = 0;
        }
    }

    fn microphone() -> Vec<i16> {
        // Exact 1 kHz periodic waveform at 16 kHz, avoiding float quantization.
        let wave = [
            0, 6269, 11585, 15137, 16384, 15137, 11585, 6269, 0, -6269, -11585, -15137, -16384, -15137, -11585, -6269,
        ];
        wave.into_iter().cycle().take(16000).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn irregular_microphone_chunks_produce_fifty_paced_reply_frames() {
        let mut capture = Capture::new(50);
        capture.capture(&microphone());
        assert_eq!(capture.count, 50);
        assert_eq!(capture.frame.len(), 0);
        assert_eq!(capture.packets.drop_count(), 0);
        let mut decoder = OpusDecoder::new().unwrap();
        let mut reply = converter(48000, 24000);
        let mut scratch = vec![0; reply.output_capacity()];
        let mut pcm = [0; 960];
        let mut bytes = Vec::new();
        let mut pacer = Pacer::new();
        let start = tokio::time::Instant::now();
        for index in 0..50 {
            let (id, packet) = pacer.next_frame(&mut capture.packets).await.unwrap();
            assert_eq!(id, index);
            assert_eq!(opus::packet::get_nb_samples(&packet, 48000).unwrap(), 960);
            assert_eq!(decoder.decode(&packet, &mut pcm).unwrap(), 960);
            let progress = reply.process(&pcm, &mut scratch).unwrap();
            assert_eq!(progress.consumed, 960);
            bytes.extend(
                scratch[..progress.written]
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes()),
            );
        }
        loop {
            let count = reply.finish(&mut scratch).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend(scratch[..count].iter().flat_map(|sample| sample.to_le_bytes()));
        }
        assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(1));
        assert_eq!(bytes.len(), 48000);
        assert_eq!(capture.packets.pop(), None);
        assert!(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .any(|pair| i16::from_le_bytes([pair[0], pair[1]]) != 0)
        );
        writeln!(
            std::io::stdout(),
            "pipeline_frames=50 reply_samples=24000 reply_s16le_bytes=48000 simulated_ms=1000"
        )
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn overload_retains_complete_newest_frames_and_reset_discards_old_stream() {
        let mut capture = Capture::new(3);
        capture.capture(&microphone());
        assert_eq!(capture.packets.len(), 3);
        assert_eq!(capture.packets.drop_count(), 47);
        let mut decoder = OpusDecoder::new().unwrap();
        let mut pacer = Pacer::new();
        for expected in 47..50 {
            let (id, packet) = pacer.next_frame(&mut capture.packets).await.unwrap();
            assert_eq!(id, expected);
            assert_eq!(decoder.decode(&packet, &mut [0; 960]).unwrap(), 960);
        }
        assert_eq!(pacer.next_frame(&mut capture.packets).await, None);
        capture.reset();
        capture.capture(&microphone());
        assert_eq!(capture.packets.drop_count(), 47);
        capture.reset();
        assert_eq!(capture.packets.len(), 0);
        assert_eq!(capture.packets.drop_count(), 0);
        capture.capture(&[0; 320]);
        let (_, packet) = capture.packets.pop().unwrap();
        let mut silence = [1; 960];
        assert_eq!(OpusDecoder::new().unwrap().decode(&packet, &mut silence).unwrap(), 960);
        assert_eq!(silence, [0; 960]);
        assert_eq!(capture.packets.pop(), None);
        // Discard both sub-frame resampling state and stale encoded entries.
        capture.reset();
        capture.capture(&[1234; 1]);
        assert_eq!(capture.frame.len(), 3);
        capture.reset();
        capture.capture(&[]);
        assert_eq!(capture.count, 0);
        assert_eq!(capture.frame.len(), 0);
        writeln!(
            std::io::stdout(),
            "backlog_capacity=3 retained_ids=[47,48,49] drops=47 fresh_stream_silence_samples=960"
        )
        .unwrap();
    }
}
