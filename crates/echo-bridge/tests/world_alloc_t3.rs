#![cfg(test)]
//! T3 allocation witness. Run this isolated binary with `make test-alloc`.
use std::{future::Future, io::Write, sync::Arc, time::Duration};
use tokio::runtime::Runtime;

#[path = "world_alloc_t3/core.rs"]
mod core;
#[path = "world_alloc_t3/device.rs"]
mod device;
#[path = "world_alloc_t3/media.rs"]
mod media;
#[path = "../src/bridge_runtime/bridge_pacing/reply_lips.rs"]
mod reply_lips;
#[path = "world_alloc_t3/screen.rs"]
mod screen;
#[path = "../src/bridge_voice/controls/world/voice_reducers.rs"]
mod voice_reducers;

use crate::core::Witness;

const WARM: usize = 64;
const FRAMES: usize = 500;
const FRAME_TIME: Duration = Duration::from_millis(20);

struct ResultRow {
    path: &'static str,
    fence: &'static str,
    steps: usize,
    core: u64,
    outside: u64,
}

fn measure(
    runtime: &Runtime,
    world: &Witness,
    path: &'static str,
    fence: &'static str,
    work: impl Future<Output = ()>,
) -> ResultRow {
    world.reset_counts();
    let total = allocation_counter::measure(|| runtime.block_on(work)).count_total;
    let (steps, core) = world.counts();
    assert!(steps >= FRAMES, "{path} sampled only {steps} steps");
    ResultRow {
        path,
        fence,
        steps,
        core,
        outside: total.checked_sub(core).unwrap(),
    }
}

#[test]
#[ignore = "isolated T3 allocation gate, run once by make test-alloc"]
fn t3_world_audio_after_warmup() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    // Keep Tokio from jumping to connection deadlines while loopback I/O
    // becomes readable. Media packet time advances explicitly.
    let driver = runtime.spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let world = Arc::new(runtime.block_on(async { Witness::new() }));
    let mut device = runtime.block_on(device::Device::open(world.clone()));
    world.open_paths();
    let _ = allocation_counter::measure(|| {});
    let [capture, mute] = device.capture_rows(&runtime, &world);

    let mut media = runtime.block_on(media::Media::open());
    let [uplink, downlink] = media.rows(&runtime, &world);
    runtime.block_on(media.close());
    let pacing = core::pacing(&runtime, &world);

    let mut screen = runtime.block_on(screen::Screen::open());
    let playback = device.playback_row(&runtime, &world, &mut screen);
    runtime.block_on(screen.close());
    let barge = core::barge(&runtime, &world);
    driver.abort();
    let rows = [capture, uplink, downlink, pacing, playback, mute, barge];
    for row in &rows {
        writeln!(
            std::io::stdout().lock(),
            "ALLOC path={} frames={FRAMES} steps={} core={} fenced={} scope={}",
            row.path,
            row.steps,
            row.core,
            row.outside,
            row.fence
        )
        .unwrap();
    }
    for row in rows {
        assert_eq!(row.core, 0, "{} core allocations", row.path);
    }
}
