//! The `servo_media::Backend` Servo is given in place of its dummy.
//!
//! Audio output and the players behind `<audio>` and `<video>` are implemented
//! here. WebRTC, capture and media streams are passed to the dummy backend, so
//! they behave as they did.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use servo_base::generic_channel::GenericCallback;
use servo_media::audio::context::{AudioContext, AudioContextOptions};
use servo_media::audio::decoder::AudioDecoder;
use servo_media::audio::sink::AudioSinkError;
use servo_media::audio::{AudioBackend, AudioStreamReader};
use servo_media::player::context::PlayerGLContext;
use servo_media::player::{audio, Player, PlayerEvent, StreamType};
use servo_media::streams::capture::MediaTrackConstraintSet;
use servo_media::streams::device_monitor::MediaDeviceMonitor;
use servo_media::streams::registry::MediaStreamId;
use servo_media::streams::{MediaOutput, MediaSocket, MediaStreamType};
use servo_media::webrtc::{WebRtcController, WebRtcSignaller};
use servo_media::{Backend, BackendMsg, ClientContextId, MediaInstance, SupportsMediaType};
use servo_media_dummy::{DummyBackend, DummyStreamReader};

use super::decode;
use super::decoder::SymphoniaAudioDecoder;
use super::mixer::Mixer;
use super::player::{GodotPlayer, Renderer};
use super::sink::GodotAudioSink;
use super::video;

type Instances = HashMap<ClientContextId, Vec<(usize, Weak<Mutex<dyn MediaInstance>>)>>;

pub struct GodotBackend {
    dummy: DummyBackend,
    /// The media instances of each page, for `mute`, `suspend` and `resume`.
    instances: Arc<Mutex<Instances>>,
    /// Where a media instance reports that it is going away.
    shutdown_channel: Arc<Mutex<Sender<BackendMsg>>>,
    next_instance_id: AtomicUsize,
}

impl GodotBackend {
    pub fn new() -> Self {
        let instances = Arc::new(Mutex::new(Instances::new()));
        let (sender, receiver) = mpsc::channel::<BackendMsg>();

        // An instance's `Drop` waits for this acknowledgement. The thread ends
        // when the backend, and with it the sender, is gone; `ServoMedia` keeps
        // the backend for the life of the process, so in practice it never does.
        let thread_instances = instances.clone();
        thread::Builder::new()
            .name("GodotMediaBackend".to_owned())
            .spawn(move || {
                while let Ok(BackendMsg::Shutdown {
                    context,
                    id,
                    tx_ack,
                }) = receiver.recv()
                {
                    let mut instances = thread_instances.lock().unwrap();
                    if let Some(list) = instances.get_mut(&context) {
                        list.retain(|(instance_id, _)| *instance_id != id);
                        if list.is_empty() {
                            instances.remove(&context);
                        }
                    }
                    drop(instances);
                    let _ = tx_ack.send(());
                }
            })
            .expect("failed to start the media backend thread");

        Self {
            dummy: DummyBackend,
            instances,
            shutdown_channel: Arc::new(Mutex::new(sender)),
            next_instance_id: AtomicUsize::new(1),
        }
    }

    fn register(
        &self,
        context: &ClientContextId,
        id: usize,
        instance: Weak<Mutex<dyn MediaInstance>>,
    ) {
        let mut instances = self.instances.lock().unwrap();
        let list = instances.entry(*context).or_default();
        // Players do not report their end the way an AudioContext does, so the
        // ones that are gone are dropped here.
        list.retain(|(_, weak)| weak.strong_count() > 0);
        list.push((id, instance));
    }

    fn for_each_instance(&self, context: &ClientContextId, f: impl Fn(&dyn MediaInstance)) {
        // Collected first: an instance being dropped waits on the backend
        // thread, which needs this lock.
        let instances: Vec<_> = self
            .instances
            .lock()
            .unwrap()
            .get(context)
            .map(|list| list.iter().filter_map(|(_, weak)| weak.upgrade()).collect())
            .unwrap_or_default();
        for instance in instances {
            f(&*instance.lock().unwrap());
        }
    }
}

impl Backend for GodotBackend {
    fn create_player(
        &self,
        id: &ClientContextId,
        stream_type: StreamType,
        sender: GenericCallback<PlayerEvent>,
        video_renderer: Option<Renderer>,
        audio_renderer: Option<Arc<Mutex<dyn audio::AudioRenderer>>>,
        gl_context: Box<dyn PlayerGLContext>,
    ) -> Arc<Mutex<dyn Player>> {
        let Some(mixer) = Mixer::get() else {
            return self.dummy.create_player(
                id,
                stream_type,
                sender,
                video_renderer,
                audio_renderer,
                gl_context,
            );
        };
        if audio_renderer.is_some() {
            log::warn!(
                "godot-servo: MediaElementAudioSourceNode is not supported; the element plays                  directly"
            );
        }
        let instance_id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let player = Arc::new(Mutex::new(GodotPlayer::new(
            instance_id,
            mixer,
            sender,
            video_renderer,
        )));
        let instance: Arc<Mutex<dyn MediaInstance>> = player.clone();
        self.register(id, instance_id, Arc::downgrade(&instance));
        player
    }

    fn create_audiostream(&self) -> MediaStreamId {
        self.dummy.create_audiostream()
    }

    fn create_videostream(&self) -> MediaStreamId {
        self.dummy.create_videostream()
    }

    fn create_stream_output(&self) -> Box<dyn MediaOutput> {
        self.dummy.create_stream_output()
    }

    fn create_stream_and_socket(
        &self,
        ty: MediaStreamType,
    ) -> (Box<dyn MediaSocket>, MediaStreamId) {
        self.dummy.create_stream_and_socket(ty)
    }

    fn create_audioinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        self.dummy.create_audioinput_stream(set)
    }

    fn create_videoinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        self.dummy.create_videoinput_stream(set)
    }

    fn create_audio_context(
        &self,
        id: &ClientContextId,
        options: AudioContextOptions,
    ) -> Result<Arc<Mutex<AudioContext>>, AudioSinkError> {
        let instance_id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let context = Arc::new(Mutex::new(AudioContext::new::<Self>(
            instance_id,
            id,
            self.shutdown_channel.clone(),
            options,
        )?));
        let instance: Arc<Mutex<dyn MediaInstance>> = context.clone();
        self.register(id, instance_id, Arc::downgrade(&instance));
        Ok(context)
    }

    fn create_webrtc(&self, signaller: Box<dyn WebRtcSignaller>) -> WebRtcController {
        self.dummy.create_webrtc(signaller)
    }

    fn can_play_type(&self, media_type: &str) -> SupportsMediaType {
        // The surer of the two: symphonia's audio, and the platform's video.
        match (
            decode::can_play_type(media_type),
            video::can_play_type(media_type),
        ) {
            (SupportsMediaType::Probably, _) | (_, SupportsMediaType::Probably) => {
                SupportsMediaType::Probably
            }
            (SupportsMediaType::Maybe, _) | (_, SupportsMediaType::Maybe) => {
                SupportsMediaType::Maybe
            }
            _ => SupportsMediaType::No,
        }
    }

    fn mute(&self, id: &ClientContextId, val: bool) {
        self.for_each_instance(id, |instance| {
            let _ = instance.mute(val);
        });
    }

    fn suspend(&self, id: &ClientContextId) {
        self.for_each_instance(id, |instance| {
            let _ = instance.suspend();
        });
    }

    fn resume(&self, id: &ClientContextId) {
        self.for_each_instance(id, |instance| {
            let _ = instance.resume();
        });
    }

    fn get_device_monitor(&self) -> Box<dyn MediaDeviceMonitor> {
        self.dummy.get_device_monitor()
    }
}

impl AudioBackend for GodotBackend {
    type Sink = GodotAudioSink;

    fn make_decoder() -> Box<dyn AudioDecoder> {
        Box::new(SymphoniaAudioDecoder)
    }

    fn make_sink() -> Result<Self::Sink, AudioSinkError> {
        Ok(GodotAudioSink::default())
    }

    fn make_streamreader(
        _id: MediaStreamId,
        _sample_rate: f32,
    ) -> Result<Box<dyn AudioStreamReader + Send>, AudioSinkError> {
        Ok(Box::new(DummyStreamReader))
    }
}
