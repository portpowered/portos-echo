// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host incoming.rs; Modified by Port OS.
use crate::media_queue::Queue;
use rtc::interceptor::{Interceptor, NoopInterceptor, Packet, StreamInfo, TaggedPacket};
use rtc::sansio::Protocol;
use rtc::shared::error::Error;
use std::{sync::Arc, time::Instant};

pub(crate) struct Ingress {
    next: NoopInterceptor,
    queue: Arc<Queue<rtc::rtp::Packet>>,
}
impl Ingress {
    pub(crate) fn new(queue: Arc<Queue<rtc::rtp::Packet>>) -> Self {
        Self {
            next: NoopInterceptor::new(),
            queue,
        }
    }
}
impl Protocol<TaggedPacket, TaggedPacket, ()> for Ingress {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Time = Instant;
    type Error = Error;
    fn handle_read(&mut self, message: TaggedPacket) -> Result<(), Error> {
        if let Packet::Rtp(packet) = message.message.packet {
            self.queue.push(packet);
        }
        Ok(())
    }
    fn poll_read(&mut self) -> Option<TaggedPacket> {
        None
    }
    fn handle_write(&mut self, message: TaggedPacket) -> Result<(), Error> {
        self.next.handle_write(message)
    }
    fn poll_write(&mut self) -> Option<TaggedPacket> {
        self.next.poll_write()
    }
}
impl Interceptor for Ingress {
    fn bind_local_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _info: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _info: &StreamInfo) {}
}
