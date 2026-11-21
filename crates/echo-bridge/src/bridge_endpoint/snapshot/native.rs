use super::{CameraSource, CaptureFuture, Sample, Shutter};
use echo_camera::Observation;
use std::{
    fs::File,
    sync::{Arc, Mutex, Weak},
};
use tokio::{io::unix::AsyncFd, sync::watch, time::Instant};

pub(in crate::bridge_endpoint) struct NativeCamera {
    exchange: echo_screen_server::Camera,
    id: String,
    active: Mutex<Weak<Switch>>,
}

impl NativeCamera {
    pub fn new(exchange: echo_screen_server::Camera, id: String) -> Self {
        Self {
            exchange,
            id,
            active: Mutex::default(),
        }
    }
}

impl CameraSource for NativeCamera {
    fn available(&self) -> bool {
        self.exchange.connected()
    }

    fn shutter(&self) -> watch::Receiver<Shutter> {
        let Some(evdev) = Observation::open() else {
            return watch::channel(Shutter::Unknown).1;
        };
        let first = evdev.check();
        if first != Shutter::Open {
            return watch::channel(first).1;
        }
        let Some(notify) = evdev.readiness() else {
            return watch::channel(Shutter::Unknown).1;
        };
        let state = Arc::new(Switch {
            evdev,
            observed: Mutex::new(first),
        });
        let Ok(mut active) = self.active.lock() else {
            return watch::channel(Shutter::Unknown).1;
        };
        *active = Arc::downgrade(&state);
        let (updates, rx) = watch::channel(first);
        tokio::spawn(watch_switch(notify, state, updates));
        rx
    }

    fn shutter_now(&self) -> Shutter {
        self.active
            .lock()
            .ok()
            .and_then(|active| active.upgrade())
            .map_or(Shutter::Unknown, |input| input.observe(Observation::check))
    }

    fn capture(&self, deadline: Instant) -> CaptureFuture<'_> {
        Box::pin(async move {
            let sample = self.exchange.capture(&self.id, deadline).await?;
            Ok(Sample {
                jpeg: sample.jpeg,
                width: sample.width,
                height: sample.height,
                sampled_at: sample.sampled_at,
            })
        })
    }
}

struct Switch {
    evdev: Observation,
    observed: Mutex<Shutter>,
}

impl Switch {
    fn observe(&self, read: fn(&Observation) -> Shutter) -> Shutter {
        let Ok(mut observed) = self.observed.lock() else {
            return Shutter::Unknown;
        };
        if *observed == Shutter::Open {
            *observed = read(&self.evdev);
        }
        *observed
    }
}

async fn watch_switch(notify: AsyncFd<File>, input: Arc<Switch>, updates: watch::Sender<Shutter>) {
    loop {
        let state = tokio::select! {
            () = updates.closed() => return,
            ready = notify.readable() => match ready {
                Ok(mut ready) => {
                    let state = input.observe(Observation::transitions);
                    ready.clear_ready();
                    state
                }
                Err(_) => Shutter::Unknown,
            }
        };
        if state != Shutter::Open {
            updates.send_replace(state);
            updates.closed().await;
            return;
        }
    }
}
