//! `ServoServer`, the engine singleton that owns the process's one `Servo`.
//!
//! Servo can only be built once per process. `Servo::new` sets options that
//! cannot be set a second time, and SpiderMonkey cannot start again once it has
//! shut down. So Servo is not tied to the nodes: the first `ServoWebView` to
//! start builds it, and it lives until the extension is deinitialized, across
//! any number of scene changes.
//!
//! The settings here are read once, when Servo is built. Set them before the
//! first `ServoWebView` starts, from an autoload for instance.

use std::cell::RefCell;

use godot::classes::Engine;
use godot::prelude::*;
use servo::{Preferences, Servo, ServoBuilder};

use crate::gl_guard::HostContext;
use crate::waker::GodotWaker;

const SINGLETON_NAME: &str = "ServoServer";

thread_local! {
    static SINGLETON: RefCell<Option<Gd<ServoServer>>> = const { RefCell::new(None) };
}

/// Called when the extension reaches the `Scene` level.
pub fn register() {
    let server = ServoServer::new_alloc();
    Engine::singleton().register_singleton(SINGLETON_NAME, &server);
    SINGLETON.with(|singleton| *singleton.borrow_mut() = Some(server));
}

/// Called when the extension leaves the `Scene` level.
///
/// Godot has deleted the `SceneTree` by then, and with it every node, but not
/// yet the rendering server or the library this code lives in. That is the
/// window for shutting Servo down: its threads have to stop before the library
/// is unloaded under them.
pub fn unregister() {
    let Some(mut server) = SINGLETON.with(|singleton| singleton.borrow_mut().take()) else {
        return;
    };
    server.bind_mut().shut_down();
    Engine::singleton().unregister_singleton(SINGLETON_NAME);
    server.free();
}

/// Owns Servo and the settings it is built with.
#[derive(GodotClass)]
#[class(base=Object, init)]
pub struct ServoServer {
    base: Base<Object>,

    /// Enable WebGL 2.0. Servo has it off by default.
    #[var(set = set_enable_webgl2)]
    #[init(val = true)]
    enable_webgl2: bool,

    servo: Option<Servo>,
    waker: GodotWaker,
}

#[godot_api]
impl ServoServer {
    #[func]
    fn set_enable_webgl2(&mut self, value: bool) {
        if value != self.enable_webgl2 && !self.refuse_change("enable_webgl2") {
            self.enable_webgl2 = value;
        }
    }
}

impl ServoServer {
    /// `None` only outside the `Scene` level, where no node can run.
    pub fn singleton() -> Option<Gd<Self>> {
        SINGLETON.with(|singleton| singleton.borrow().clone())
    }

    /// Servo, built on the first call.
    pub fn servo(&mut self) -> Servo {
        if let Some(servo) = &self.servo {
            return servo.clone();
        }

        install_crypto_provider();
        let preferences = Preferences {
            dom_webgl2_enabled: self.enable_webgl2,
            ..Default::default()
        };
        let servo = ServoBuilder::default()
            .preferences(preferences)
            .event_loop_waker(Box::new(self.waker.clone()))
            .build();
        servo.setup_logging();

        self.servo = Some(servo.clone());
        servo
    }

    fn shut_down(&mut self) {
        if let Some(servo) = self.servo.take() {
            // Dropping Servo sends Exit and spins the event loop until the
            // constellation is gone, which makes Servo's GL context current.
            let _host_context = HostContext::capture();
            drop(servo);
        }
    }

    /// Whether a setting has to stay as it is. Warns when it does.
    fn refuse_change(&self, property: &str) -> bool {
        if self.servo.is_none() {
            return false;
        }
        godot_warn!(
            "godot-servo: ServoServer.{property} is read once, when Servo starts, and it \
             already has. Set it before the first ServoWebView starts, from an autoload for \
             instance."
        );
        true
    }
}

/// Servo's network layer assumes a rustls provider has been installed.
fn install_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}
