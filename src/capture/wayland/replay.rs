//! Paced replay of the last completed EXT capture while the source is quiet.
use std::time::Instant;

use ironrdp_server::DisplayUpdate;
use tokio::sync::mpsc;

use crate::capture::frame::{FramePacer, FrameProcessor};
use crate::capture::scale::PreparedPresentationFrame;

pub(in crate::capture) struct CaptureReplay<'a> {
    latest: Option<PreparedPresentationFrame<'a>>,
}

impl<'a> CaptureReplay<'a> {
    pub(in crate::capture) fn new() -> Self {
        Self { latest: None }
    }

    /// End the borrow before this buffer can be handed to the compositor again.
    pub(in crate::capture) fn clear(&mut self) {
        self.latest = None;
    }

    pub(in crate::capture) fn submit(
        &mut self,
        frame: PreparedPresentationFrame<'a>,
        processor: &mut FrameProcessor,
        pacer: &mut FramePacer,
        tx: &mpsc::Sender<DisplayUpdate>,
        now: Instant,
    ) -> bool {
        if !processor.has_pending_work() {
            processor
                .stats
                .record_no_damage_skip(frame.width, frame.height);
        } else if !pacer.should_send(
            now,
            processor.sent_first_frame,
            true,
            processor.pacing_fps(),
        ) {
            processor.stats.record_pacer_skip(frame.width, frame.height);
            self.latest = Some(frame);
            return true;
        } else if !processor.process_at(frame.data.as_ref(), tx, now) {
            self.clear();
            return false;
        }
        // `process_at(true)` also means a temporary refusal, such as a full
        // ACK window. Keep the pixels after success too: a later full refresh
        // or GFX activation must not depend on new compositor damage.
        self.latest = Some(frame);
        true
    }

    pub(in crate::capture) fn poll(
        &mut self,
        processor: &mut FrameProcessor,
        pacer: &mut FramePacer,
        tx: &mpsc::Sender<DisplayUpdate>,
        now: Instant,
    ) -> bool {
        if let Some(frame) = self.latest.as_ref() {
            if processor.has_pending_work()
                && pacer.should_send(
                    now,
                    processor.sent_first_frame,
                    true,
                    processor.pacing_fps(),
                )
                && !processor.process_at(frame.data.as_ref(), tx, now)
            {
                self.clear();
                return false;
            }
        }
        true
    }
}
