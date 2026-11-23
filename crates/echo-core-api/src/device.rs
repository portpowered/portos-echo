/// Synchronous device observations. Transport acknowledgements precede these events.
#[derive(Clone, Copy)]
pub enum DeviceEvent {
    Connected,
    Disconnected,
    Registered { registered: bool },
    RunOpened,
    RunClosed,
    CaptureChanged { open: bool },
    MicMuteObserved { muted: bool },
    SpeakerObserved { volume: Option<u8>, muted: Option<bool> },
}

/// The device policy decisions applied by the link service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceView {
    pub link: LinkState,
    pub run_open: bool,
    pub capture_open: bool,
    pub privacy: CapturePrivacy,
    pub speaker_volume: Option<u8>,
    pub speaker_muted: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkState {
    #[default]
    Disconnected,
    Connected,
    Registered,
    Paused,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CapturePrivacy {
    pub mic_muted: bool,
    pub mute_latched: bool,
}

/// The bridge injects its World here. Standalone link clients use the same
/// device transitions with a private state instance.
pub trait DeviceControl: Send + Sync {
    fn connect(&self) -> (u64, DeviceView);
    fn observe(&self, generation: u64, change: DeviceEvent) -> DeviceView;
}

#[derive(Default)]
pub struct DeviceState {
    generation: u64,
    view: DeviceView,
}

impl DeviceState {
    #[must_use]
    pub fn next_generation(&self) -> u64 {
        self.generation.saturating_add(1)
    }

    #[must_use]
    pub fn current(&self) -> DeviceView {
        self.view
    }

    #[must_use]
    pub fn view(&self, generation: u64) -> DeviceView {
        if self.generation == generation {
            self.view
        } else {
            DeviceView::default()
        }
    }

    /// Applies observations shared by the bridge reducer and standalone links.
    pub fn apply(&mut self, generation: u64, change: DeviceEvent) -> Option<DeviceView> {
        if matches!(change, DeviceEvent::Connected) && generation > self.generation {
            self.generation = generation;
            self.view = DeviceView {
                link: LinkState::Connected,
                ..DeviceView::default()
            };
            return Some(self.view);
        }
        if self.generation != generation || self.view.link == LinkState::Disconnected {
            return None;
        }
        match change {
            DeviceEvent::Connected => return None,
            DeviceEvent::Disconnected => self.view = DeviceView::default(),
            DeviceEvent::Registered { registered } => {
                self.view.link = if registered {
                    LinkState::Registered
                } else {
                    LinkState::Paused
                };
            }
            DeviceEvent::RunOpened => {
                self.view.run_open = true;
                self.view.privacy.mute_latched = self.view.privacy.mic_muted;
                self.view.capture_open = !self.view.privacy.mic_muted;
            }
            DeviceEvent::RunClosed => {
                self.view.run_open = false;
                self.view.capture_open = false;
                self.view.privacy.mute_latched = self.view.privacy.mic_muted;
            }
            DeviceEvent::CaptureChanged { open } => self.view.capture_open = open,
            DeviceEvent::MicMuteObserved { muted } => {
                self.view.privacy.mic_muted = muted;
                if muted {
                    self.view.capture_open = false;
                    self.view.privacy.mute_latched = true;
                }
            }
            DeviceEvent::SpeakerObserved { volume, muted } => {
                self.view.speaker_volume = volume;
                self.view.speaker_muted = muted;
            }
        }
        Some(self.view)
    }
}
