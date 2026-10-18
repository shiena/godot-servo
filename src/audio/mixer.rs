//! Sums every playing source into the stereo stream Godot plays.
//!
//! Each source has its own queue, already at the output rate: resampling and
//! channel mixing happen on the thread that produces the samples, so `pull` only
//! copies and adds.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub type Frame = [f32; 2];

/// The queue length a source aims for before the output has reported a frame
/// time: about 20 ms at 48 kHz.
const DEFAULT_HIGH_WATER: usize = 1024;

static MIXER: OnceLock<Mixer> = OnceLock::new();

pub struct Mixer {
    rate: u32,
    sources: Mutex<Vec<Arc<Source>>>,
    /// How many frames each source should keep queued. The output sets it from
    /// the frame time, so a source holds enough to cover one frame.
    high_water: AtomicUsize,
    /// How long a pulled frame takes to be heard, in microseconds: what the
    /// output still holds plus the audio driver's latency.
    latency_us: AtomicU64,
}

impl Mixer {
    /// Create the process's mixer. `false` when it already exists.
    pub fn init(rate: u32) -> bool {
        MIXER
            .set(Mixer {
                rate,
                sources: Mutex::new(Vec::new()),
                high_water: AtomicUsize::new(DEFAULT_HIGH_WATER),
                latency_us: AtomicU64::new(0),
            })
            .is_ok()
    }

    pub fn get() -> Option<&'static Mixer> {
        MIXER.get()
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn high_water(&self) -> usize {
        self.high_water.load(Ordering::Relaxed)
    }

    pub fn set_high_water(&self, frames: usize) {
        self.high_water.store(frames, Ordering::Relaxed);
    }

    pub fn latency(&self) -> f64 {
        self.latency_us.load(Ordering::Relaxed) as f64 / 1e6
    }

    pub fn set_latency(&self, seconds: f64) {
        self.latency_us
            .store((seconds.max(0.0) * 1e6) as u64, Ordering::Relaxed);
    }

    /// A new source. `wake` is called from `pull` whenever it leaves the
    /// source's queue below the high-water mark, so the producer tops it up
    /// before the next pull.
    pub fn add_source(&'static self, wake: Box<dyn Fn() + Send + Sync>) -> SourceHandle {
        let source = Arc::new(Source {
            frames: Mutex::new(VecDeque::new()),
            gain: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            primed: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            consumed: AtomicU64::new(0),
            wake,
        });
        self.sources.lock().unwrap().push(source.clone());
        SourceHandle {
            mixer: self,
            source,
        }
    }

    /// Mix the next `frames` frames into `out`, replacing what it held.
    pub fn pull(&self, frames: usize, out: &mut Vec<Frame>) {
        out.clear();
        out.resize(frames, [0.0; 2]);
        let high_water = self.high_water();
        // Cloned so a producer adding or dropping a source does not wait for the
        // mixing, and `wake` runs without the list locked.
        let sources: Vec<Arc<Source>> = self.sources.lock().unwrap().clone();
        for source in sources {
            if source.mix_into(out, high_water) {
                (source.wake)();
            }
        }
    }
}

pub struct Source {
    frames: Mutex<VecDeque<Frame>>,
    gain: AtomicU32,
    muted: AtomicBool,
    playing: AtomicBool,
    /// Whether the source played in the last pull. A source that has not starts
    /// again only once it has a pull's worth queued, so it does not stutter.
    primed: AtomicBool,
    /// Nothing more is coming, so what is queued plays without waiting for a
    /// pull's worth.
    finished: AtomicBool,
    consumed: AtomicU64,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Source {
    /// Add this source's next frames to `out`. Returns whether it wants more.
    fn mix_into(&self, out: &mut [Frame], high_water: usize) -> bool {
        if !self.playing.load(Ordering::Relaxed) {
            return false;
        }
        let wanted = out.len();
        let mut frames = self.frames.lock().unwrap();
        let queued = frames.len();
        let primed = self.primed.load(Ordering::Relaxed);
        if queued == 0 {
            self.primed.store(false, Ordering::Relaxed);
            return !self.finished.load(Ordering::Relaxed);
        }
        if !primed && !self.finished.load(Ordering::Relaxed) && queued < wanted.min(high_water) {
            return true;
        }

        let taken = queued.min(wanted);
        // A source that is starting is placed at the end of the window, so the
        // silence before it is just a later start. One that ran dry stops where
        // its frames end: there is nothing better to do with an underrun.
        let start = if primed { 0 } else { wanted - taken };
        let gain = if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(self.gain.load(Ordering::Relaxed))
        };
        for (slot, frame) in out[start..start + taken]
            .iter_mut()
            .zip(frames.drain(..taken))
        {
            slot[0] += frame[0] * gain;
            slot[1] += frame[1] * gain;
        }
        self.primed.store(taken == wanted, Ordering::Relaxed);
        self.consumed.fetch_add(taken as u64, Ordering::Relaxed);
        frames.len() < high_water
    }
}

/// The producer's end of a source. Dropping it takes the source out of the mix.
pub struct SourceHandle {
    mixer: &'static Mixer,
    source: Arc<Source>,
}

impl SourceHandle {
    pub fn mixer(&self) -> &'static Mixer {
        self.mixer
    }

    pub fn push(&self, frames: impl IntoIterator<Item = Frame>) {
        self.source.frames.lock().unwrap().extend(frames);
    }

    pub fn queued(&self) -> usize {
        self.source.frames.lock().unwrap().len()
    }

    /// Frames the mix has taken from this source so far.
    pub fn consumed(&self) -> u64 {
        self.source.consumed.load(Ordering::Relaxed)
    }

    /// Start or stop taking part in the mix. What is queued stays queued.
    pub fn set_playing(&self, playing: bool) {
        self.source.playing.store(playing, Ordering::Relaxed);
        if !playing {
            self.source.primed.store(false, Ordering::Relaxed);
        }
    }

    /// Drop what is queued.
    pub fn clear(&self) {
        self.source.frames.lock().unwrap().clear();
        self.source.primed.store(false, Ordering::Relaxed);
    }

    pub fn set_finished(&self, finished: bool) {
        self.source.finished.store(finished, Ordering::Relaxed);
    }

    pub fn set_gain(&self, gain: f32) {
        self.source.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn set_muted(&self, muted: bool) {
        self.source.muted.store(muted, Ordering::Relaxed);
    }
}

impl Drop for SourceHandle {
    fn drop(&mut self) {
        self.mixer
            .sources
            .lock()
            .unwrap()
            .retain(|source| !Arc::ptr_eq(source, &self.source));
    }
}
