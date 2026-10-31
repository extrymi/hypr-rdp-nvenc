//! SHM dispatch/pacing shared by the EXT caller and its CPU regression tests.
use std::time::Instant;

use ironrdp_server::DisplayUpdate;
use tokio::sync::mpsc;

use crate::capture::frame::{FramePacer, FrameProcessor};
use crate::capture::scale::PreparedPresentationFrame;

pub(in crate::capture) struct CaptureReplay<'a> {
    deferred: Option<PreparedPresentationFrame<'a>>,
}

impl<'a> CaptureReplay<'a> {
    pub(in crate::capture) fn new() -> Self {
        Self { deferred: None }
    }

    /// End the borrow before this buffer can be handed to the compositor again.
    pub(in crate::capture) fn clear(&mut self) {
        self.deferred = None;
    }

    pub(in crate::capture) fn submit(
        &mut self,
        frame: PreparedPresentationFrame<'a>,
        processor: &mut FrameProcessor,
        pacer: &mut FramePacer,
        tx: &mpsc::Sender<DisplayUpdate>,
        now: Instant,
    ) -> bool {
        self.clear();
        let pending = processor.has_pending_damage();
        if !pending && processor.sent_first_frame {
            processor
                .stats
                .record_no_damage_skip(frame.width, frame.height);
        } else if pacer.should_send(
            now,
            processor.sent_first_frame,
            pending,
            processor.pacing_fps(),
        ) {
            if !processor.process(frame.data.as_ref(), tx) {
                return false;
            }
        } else {
            processor.stats.record_pacer_skip(frame.width, frame.height);
            self.deferred = Some(frame);
        }
        true
    }

    pub(in crate::capture) fn poll(
        &mut self,
        processor: &mut FrameProcessor,
        pacer: &mut FramePacer,
        tx: &mpsc::Sender<DisplayUpdate>,
        now: Instant,
    ) -> bool {
        if let Some(frame) = self.deferred.as_ref() {
            if pacer.should_send(
                now,
                processor.sent_first_frame,
                processor.has_pending_damage(),
                processor.pacing_fps(),
            ) {
                if !processor.process(frame.data.as_ref(), tx) {
                    return false;
                }
                self.deferred = None;
            }
        }
        true
    }
}
