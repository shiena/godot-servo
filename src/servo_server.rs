//! `ServoServer`, the engine singleton that owns the process's one `Servo`.
//!
//! Servo can only be built once per process. `Servo::new` sets options that
//! cannot be set a second time, and SpiderMonkey cannot start again once it has
//! shut down. So Servo is not tied to the nodes: the first `ServoWebView` to
//! start builds it, and it lives until the extension is deinitialized, across
//! any number of scene changes.
//!
//! The settings here are read once, when Servo is built. Set them before the
//! first `ServoWebView` starts: from an autoload, or from any `_ready()` in the
//! scene when the node uses `autostart`, which starts it deferred.
//!
//! Servo is pumped here as well, once per frame, from `SceneTree::process_frame`.
//! Godot emits that signal before any node's `_process`, so every node's input
//! reaches Servo before the spin and every node paints after it.

use std::cell::RefCell;

use godot::classes::{Engine, SceneTree};
use godot::prelude::*;
use servo::{Preferences, Servo, ServoBuilder};

use crate::gl_guard::HostContext;
use crate::waker::GodotWaker;
use crate::webview_node::ServoWebView;

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

    /// Enable WebGPU. Servo has it off by default.
    ///
    /// On here, because the binary carries wgpu either way, and Servo starts
    /// its WebGPU thread only on a page's first request. A `file://` page
    /// cannot use it at all; see the README.
    #[var(set = set_enable_webgpu)]
    #[init(val = true)]
    enable_webgpu: bool,

    servo: Option<Servo>,
    waker: GodotWaker,
    /// The running `ServoWebView`s. Ids rather than `Gd`s, so a node freed
    /// without leaving the tree is skipped instead of dereferenced.
    webviews: Vec<InstanceId>,
}

#[godot_api]
impl ServoServer {
    #[func]
    fn set_enable_webgl2(&mut self, value: bool) {
        if value != self.enable_webgl2 && !self.refuse_change("enable_webgl2") {
            self.enable_webgl2 = value;
        }
    }

    #[func]
    fn set_enable_webgpu(&mut self, value: bool) {
        if value != self.enable_webgpu && !self.refuse_change("enable_webgpu") {
            self.enable_webgpu = value;
        }
    }
}

impl ServoServer {
    /// `None` only outside the `Scene` level, where no node can run.
    pub fn singleton() -> Option<Gd<Self>> {
        SINGLETON.with(|singleton| singleton.borrow().clone())
    }

    /// Servo, built on the first call. `webview` is pumped from now on.
    pub fn attach(&mut self, webview: InstanceId) -> Servo {
        if !self.webviews.contains(&webview) {
            self.webviews.push(webview);
        }
        if let Some(servo) = &self.servo {
            return servo.clone();
        }

        // Servo leaves threads behind that nothing can join, so this library has
        // to stay mapped after Godot unloads it at exit. See `module_pin`.
        crate::module_pin::pin();

        install_crypto_provider();
        let preferences = Preferences {
            dom_webgl2_enabled: self.enable_webgl2,
            dom_webgpu_enabled: self.enable_webgpu,
            ..Default::default()
        };
        let servo = ServoBuilder::default()
            .preferences(preferences)
            .event_loop_waker(Box::new(self.waker.clone()))
            .build();
        servo.setup_logging();

        match Engine::singleton()
            .get_main_loop()
            .and_then(|main_loop| main_loop.try_cast::<SceneTree>().ok())
        {
            Some(tree) => {
                tree.signals()
                    .process_frame()
                    .connect_other(&self.to_gd(), Self::pump);
            }
            None => godot_error!("godot-servo: no SceneTree to pump Servo from"),
        }

        self.servo = Some(servo.clone());
        servo
    }

    /// Stop pumping `webview`. Servo itself stays up for the next one.
    pub fn detach(&mut self, webview: InstanceId) {
        self.webviews.retain(|id| *id != webview);
    }

    /// Once per frame, before any node's `_process`.
    fn pump(&mut self) {
        let Some(servo) = self.servo.clone() else {
            return;
        };
        self.webviews
            .retain(|id| Gd::<ServoWebView>::try_from_instance_id(*id).is_ok());
        let active: Vec<Gd<ServoWebView>> = self
            .webviews
            .iter()
            .filter_map(|id| Gd::<ServoWebView>::try_from_instance_id(*id).ok())
            .filter(|webview| webview.is_inside_tree() && webview.can_process())
            .collect();

        // While every node is paused, so is Servo, as it was when each node
        // pumped from its own `_process`. With no node running at all there is
        // still the last WebView's teardown to finish, so a wake-up is answered.
        let woken = self.waker.take_pending();
        if active.is_empty() && (!self.webviews.is_empty() || !woken) {
            return;
        }

        for mut webview in active {
            webview.bind_mut().before_spin();
        }
        // `spin_event_loop()` makes Servo's GL context current.
        let _host_context = HostContext::capture();
        servo.spin_event_loop();
    }

    fn shut_down(&mut self) {
        self.webviews.clear();
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
             already has. Set it before the first ServoWebView starts: from an autoload, or \
             from _ready() when the node uses autostart."
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
