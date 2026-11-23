use super::*;
use echo_core_api::{Clock, Command, DeviceEvent, DeviceView, PausedClock};
use std::time::UNIX_EPOCH;

fn apply(state: &mut DeviceState, generation: u64, change: DeviceEvent) -> Option<DeviceView> {
    let mut out = Commands::default();
    Device::on_event(
        state,
        &mut Event::Device { generation, change },
        PausedClock::new(UNIX_EPOCH).now(),
        &mut out,
    );
    out.take().map(
        |Command::ApplyDeviceView {
             generation: published,
             view,
         }| {
            assert_eq!(published, generation);
            view
        },
    )
}

#[test]
fn unmute_keeps_same_run_closed_to_pcm_until_next_wake() {
    let mut world = DeviceState::default();
    apply(&mut world, 1, DeviceEvent::Connected);
    apply(&mut world, 1, DeviceEvent::Registered { registered: true });
    assert!(apply(&mut world, 1, DeviceEvent::RunOpened).unwrap().capture_open);
    apply(&mut world, 1, DeviceEvent::MicMuteObserved { muted: true });
    let unmuted = apply(&mut world, 1, DeviceEvent::MicMuteObserved { muted: false }).unwrap();
    assert!(!unmuted.privacy.mic_muted);
    assert!(unmuted.privacy.mute_latched && unmuted.run_open && !unmuted.capture_open);
    let next = apply(&mut world, 1, DeviceEvent::RunOpened).unwrap();
    assert!(next.capture_open && !next.privacy.mute_latched);
    assert_eq!(Device::check(&world), Ok(()));
}

#[test]
fn retired_generation_cannot_close_capture_or_publish_speaker_state() {
    let mut world = DeviceState::default();
    apply(&mut world, 1, DeviceEvent::Connected);
    apply(
        &mut world,
        1,
        DeviceEvent::SpeakerObserved {
            volume: Some(63),
            muted: Some(false),
        },
    );
    apply(&mut world, 1, DeviceEvent::Disconnected);
    let connected = apply(&mut world, 2, DeviceEvent::Connected).unwrap();
    assert_eq!(
        connected,
        DeviceView {
            link: LinkState::Connected,
            ..DeviceView::default()
        }
    );
    apply(&mut world, 2, DeviceEvent::Registered { registered: true });
    let wake = apply(&mut world, 2, DeviceEvent::RunOpened).unwrap();
    assert_eq!(apply(&mut world, 1, DeviceEvent::Disconnected), None);
    assert_eq!(
        apply(
            &mut world,
            1,
            DeviceEvent::SpeakerObserved {
                volume: Some(63),
                muted: Some(false)
            }
        ),
        None
    );
    assert_eq!(world.current(), wake);
    assert_eq!(Device::check(&world), Ok(()));
}

#[test]
fn capture_without_a_run_reports_the_broken_field() {
    let mut world = DeviceState::default();
    apply(&mut world, 1, DeviceEvent::Connected);
    apply(&mut world, 1, DeviceEvent::CaptureChanged { open: true });
    assert_eq!(
        Device::check(&world),
        Err(Broken {
            field: "device.capture_open",
            expected: "false",
            actual: "true"
        })
    );
}
