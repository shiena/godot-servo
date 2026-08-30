//! Servo が「イベントループを回してくれ」と通知してくるためのフック。
//!
//! Servo は自前のスレッド群から `wake()` を呼ぶので、ここではフラグを立てるだけに
//! とどめ、実際の `spin_event_loop()` は Godot のメインスレッド (`_process`) で行う。

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

    /// フラグを読んで下ろす。立っていたら `spin_event_loop()` を呼ぶ。
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
