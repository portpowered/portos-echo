//! Private writer controls for the paused M2 loopback witness.
use super::{Owner, Trace};
use crate::{Error, world_witness::TRACE_TICK_ENV};
use serde_json::{Value, json};
use std::{cell::RefCell, future::Future, sync::Weak};
use tokio::time::Instant;

struct WriterHandle {
    owner: Weak<Owner>,
    started: Instant,
    mode: &'static str,
}

tokio::task_local! {
    static WRITER: RefCell<Option<WriterHandle>>;
}

pub(crate) async fn scope<T>(private_witness: bool, work: impl Future<Output = T>) -> T {
    if private_witness && std::env::var(TRACE_TICK_ENV).as_deref() == Ok("1") {
        WRITER.scope(RefCell::new(None), work).await
    } else {
        work.await
    }
}

pub(crate) fn attach(trace: &Trace) {
    let _ = WRITER.try_with(|writer| {
        writer.replace(Some(WriterHandle {
            owner: std::sync::Arc::downgrade(&trace.owner),
            started: trace.started,
            mode: trace.mode,
        }));
    });
}

pub(crate) fn control(hold: bool) -> Result<Value, Error> {
    WRITER
        .try_with(|writer| {
            let writer = writer.borrow();
            let writer = writer.as_ref().ok_or(Error::Control)?;
            let owner = writer.owner.upgrade().ok_or(Error::Control)?;
            owner.world.hold_trace(hold);
            if hold {
                let trace = Trace {
                    owner,
                    started: writer.started,
                    mode: writer.mode,
                    payloads: false,
                };
                trace.buffer_clock_offset(
                    json!({"synced":false,"reason":"unreachable","hosts":0,"servers":1,"corrected":false}),
                )?;
            }
            Ok(json!({"event":"bridge.trace_writer", "held":hold}))
        })
        .map_err(|_| Error::Control)?
}
