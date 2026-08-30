//! The hook Servo uses to ask for the event loop to be pumped.
//!
//! Servo calls `wake()` from its own threads, so all that happens here is
//! setting a flag. The actual `spin_event_loop()` runs on Godot's main thread,
//! in `_process`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use servo::EventLoopWaker;

#[derive(Clone, Default)]
pub struct GodotWaker {
    pending: Arc<AtomicBool>,
}

impl GodotWaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the flag and clear it. When it was set, call `spin_event_loop()`.
    pub fn take_pending(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

impl EventLoopWaker for GodotWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.pending.store(true, Ordering::Release);
    }
}
