//! `ServoWebView`, the node Godot sees.
//!
//! One node corresponds to one Servo `WebView`. There is a single `Servo`
//! instance per process, shared by every node.

use std::cell::RefCell;
use std::rc::Rc;

use dpi::PhysicalSize;
use godot::classes::notify::NodeNotification;
use godot::classes::{
    DisplayServer, INode, InputEvent, InputEventKey, InputEventMouseButton, InputEventMouseMotion,
    InputEventScreenDrag, InputEventScreenTouch, Node, Texture2D,
};
use godot::global::{Key as GodotKey, MouseButton as GodotMouseButton};
use godot::prelude::*;
use servo::{
    Code, CompositionEvent, CompositionState, DevicePoint, ImeEvent, InputEvent as ServoInputEvent,
    JSValue, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, PrefValue, Servo, ServoBuilder, TouchEvent, TouchEventType,
    TouchId, TouchPointerType, UserContentManager, UserScript, WebView, WebViewBuilder, WheelDelta,
    WheelEvent, WheelMode,
};

use crate::bridge::{self, TextureBridge};
use crate::delegate::{ServoEvent, ServoEventSink, BRIDGE_SCRIPT};
use crate::gl_guard::HostContext;
use crate::rendering_context::GodotRenderingContext;
use crate::waker::GodotWaker;

/// Pixels per wheel notch, matching the value servoshell uses.
const WHEEL_LINE_HEIGHT: f64 = 76.0;

/// The `device` on synthetic events Godot builds by converting other input.
/// `InputEvent::DEVICE_ID_EMULATION` is not exposed to GDExtension, so it is
/// written out here.
const DEVICE_ID_EMULATION: i32 = -1;

struct Inner {
    servo: Servo,
    webview: WebView,
    context: Rc<GodotRenderingContext>,
    sink: Rc<ServoEventSink>,
    waker: GodotWaker,
    bridge: Box<dyn TextureBridge>,
    _user_content: Rc<UserContentManager>,
}

#[derive(GodotClass)]
#[class(base=Node, init)]
pub struct ServoWebView {
    base: Base<Node>,

    /// The URL to open on start.
    #[export]
    #[init(val = GString::from("about:blank"))]
    url: GString,

    /// The WebView resolution, in pixels.
    #[export]
    #[init(val = Vector2i::new(1024, 768))]
    view_size: Vector2i,

    /// Whether to start automatically in `_ready()`.
    #[export]
    #[init(val = true)]
    autostart: bool,

    /// Enable WebGL 2.0. Servo has it off by default.
    #[export]
    #[init(val = true)]
    enable_webgl2: bool,

    /// Where to put the IME candidate window, in window coordinates.
    ///
    /// The caret position inside the WebView cannot be used as it is: on a 3D
    /// panel there is no correspondence between a position on the panel and one
    /// on screen. Take the caret rectangle from the `ime_requested` signal,
    /// project it in the game, and assign the result here. The default (0, 0) is
    /// the top-left of the window.
    #[export]
    ime_anchor: Vector2,

    inner: Option<Inner>,
    next_script_id: i64,

    /// Whether the IME is up. True only while an editable element has focus.
    ime_active: bool,
    /// Whether a composition is in progress, so `CompositionState::Start` is
    /// sent exactly once.
    composing: bool,
    /// The previous preedit string, used to drop notifications that changed nothing.
    last_preedit: GString,
    /// The composition ended and the committed text is expected to arrive as key events.
    awaiting_commit: bool,
    /// Where that committed text is assembled.
    pending_commit: String,
}

#[godot_api]
impl INode for ServoWebView {
    fn ready(&mut self) {
        if self.autostart {
            self.start();
        }
    }

    fn process(&mut self, _delta: f64) {
        self.pump();
    }

    fn exit_tree(&mut self) {
        self.stop();
    }

    /// The IME preedit arrives as a notification, not as an input event.
    ///
    /// The committed text comes through as ordinary `InputEventKey`s carrying a
    /// unicode value, which the existing `feed_input()` route already handles.
    fn on_notification(&mut self, what: NodeNotification) {
        if what == NodeNotification::OS_IME_UPDATE && self.ime_active {
            self.sync_preedit();
        }
    }
}

#[godot_api]
impl ServoWebView {
    // ── Signals ─────────────────────────────────────────────────────────────

    /// The texture's contents changed.
    #[signal]
    fn frame_updated();

    /// The page title changed.
    #[signal]
    fn title_changed(title: GString);

    /// The displayed URL changed.
    #[signal]
    fn url_changed(url: GString);

    #[signal]
    fn load_started();

    #[signal]
    fn load_finished();

    /// The cursor shape the page asks for changed. Useful for hover feedback.
    #[signal]
    fn cursor_changed(shape: GString);

    /// Output from `console.log` and friends.
    #[signal]
    fn console_message(level: GString, message: GString);

    /// An event the page threw at Godot.
    ///
    /// The page raises it either way:
    ///
    /// ```js
    /// godot.emit("buy", { item: "potion" });   // payload arrives as a JSON string
    /// ```
    /// ```html
    /// <a href="godot:buy?item=potion">Buy</a>  <!-- payload is the query string -->
    /// ```
    #[signal]
    fn bridge_event(name: GString, payload: GString);

    /// The result of `evaluate_javascript()`. `id` matches that call's return value.
    #[signal]
    fn script_result(id: i64, value: Variant);

    /// An editable element in the page took focus and the IME came up.
    ///
    /// `caret` is a rectangle in WebView pixels. Project it to screen coordinates
    /// and assign the result to `ime_anchor` to place the candidate window.
    #[signal]
    fn ime_requested(caret: Rect2, multiline: bool);

    /// Focus left and the IME went down.
    #[signal]
    fn ime_dismissed();

    /// The page's process died. Rendering stops; `reload()` rebuilds it.
    #[signal]
    fn crashed(reason: GString);

    /// The page called `alert()`.
    ///
    /// The page's JavaScript is blocked until it is answered, so always call
    /// `respond_to_dialog()` once the message has been shown. `message` is text
    /// the page chooses, so present it in a way that cannot be mistaken for the
    /// game's own UI.
    #[signal]
    fn dialog_alert(message: GString);

    /// The page called `confirm()`. Answer with `respond_to_dialog(accepted, "")`.
    #[signal]
    fn dialog_confirm(message: GString);

    /// The page called `prompt()`. Answer with `respond_to_dialog(accepted, text)`.
    #[signal]
    fn dialog_prompt(message: GString, default_value: GString);

    /// A `<select>` was opened. Answer with `respond_to_select()`.
    ///
    /// `options` is an array of `{ id, label, disabled, group }` dictionaries.
    /// `<optgroup>`s are flattened, with the group's name in `group`.
    #[signal]
    fn select_element_requested(options: Array<Variant>, allow_multiple: bool);

    // ── Lifetime ────────────────────────────────────────────────────────────

    #[func]
    fn start(&mut self) {
        if self.inner.is_some() {
            return;
        }
        let size = self.physical_size();

        // Load ANGLE from beside the extension before surfman goes looking for it.
        crate::angle_loader::preload();

        // Servo's GL context becomes current below. Godot's is restored on the way
        // out, which the Compatibility renderer on Android depends on.
        let _host_context = HostContext::capture();

        let context = match GodotRenderingContext::new(size) {
            Ok(context) => Rc::new(context),
            Err(error) => {
                godot_error!("godot-servo: could not create a rendering context: {error:?}");
                return;
            }
        };
        if let Err(error) = context.make_current_public() {
            godot_error!("godot-servo: could not make the GL context current: {error:?}");
            return;
        }

        let waker = GodotWaker::new();
        let servo = servo_instance::acquire(&waker);

        // Off by default in Servo. The preference is process-wide, so the first
        // node to start decides it.
        servo.set_preference("dom_webgl2_enabled", PrefValue::Bool(self.enable_webgl2));

        let user_content = Rc::new(UserContentManager::new(&servo));
        user_content.add_script(Rc::new(UserScript::new(BRIDGE_SCRIPT.to_owned(), None)));

        let sink = Rc::new(ServoEventSink::new());
        let mut builder = WebViewBuilder::new(&servo, context.clone())
            .delegate(sink.clone())
            .user_content_manager(user_content.clone());
        if let Ok(url) = url::Url::parse(&self.url.to_string()) {
            builder = builder.url(url);
        }
        let webview = builder.build();
        webview.focus();
        webview.show();

        let bridge = bridge::create(&context, size, &_host_context);
        godot_print!(
            "godot-servo: started with the '{}' texture path",
            bridge.backend_name()
        );

        self.inner = Some(Inner {
            servo,
            webview,
            context,
            sink,
            waker,
            bridge,
            _user_content: user_content,
        });
    }

    #[func]
    fn stop(&mut self) {
        if self.ime_active {
            self.set_ime_enabled(false);
        }
        if let Some(mut inner) = self.inner.take() {
            inner.bridge.release();
            drop(inner.webview);
            drop(inner.servo);
            servo_instance::release();
        }
    }

    #[func]
    fn is_running(&self) -> bool {
        self.inner.is_some()
    }

    /// The name of the texture sharing path actually in use.
    #[func]
    fn get_backend_name(&self) -> GString {
        match &self.inner {
            Some(inner) => GString::from(inner.bridge.backend_name()),
            None => GString::from("stopped"),
        }
    }

    // ── Display ─────────────────────────────────────────────────────────────

    /// What Servo rendered. Drops straight into a material or a `TextureRect`.
    #[func]
    fn get_texture(&self) -> Option<Gd<Texture2D>> {
        self.inner.as_ref().map(|inner| inner.bridge.texture())
    }

    /// Whether the texture arrives upside down. Only the macOS path returns `true`.
    #[func]
    fn is_texture_flipped_v(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.bridge.needs_v_flip())
    }

    /// Whether the texture can only be read by a shader declaring
    /// `samplerExternalOES`. Only the Android AHardwareBuffer path returns `true`.
    #[func]
    fn needs_external_sampler(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.bridge.needs_external_sampler())
    }

    // ── Navigation ──────────────────────────────────────────────────────────

    #[func]
    fn load_url(&mut self, url: GString) {
        self.url = url.clone();
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        match url::Url::parse(&url.to_string()) {
            Ok(url) => inner.webview.load(url),
            Err(error) => godot_error!("godot-servo: invalid URL {url}: {error}"),
        }
    }

    #[func]
    fn reload(&mut self) {
        if let Some(inner) = self.inner.as_ref() {
            inner.webview.reload();
        }
    }

    #[func]
    fn go_back(&mut self) {
        if let Some(inner) = self.inner.as_ref() {
            let _ = inner.webview.go_back(1);
        }
    }

    #[func]
    fn go_forward(&mut self) {
        if let Some(inner) = self.inner.as_ref() {
            let _ = inner.webview.go_forward(1);
        }
    }

    /// Evaluate JavaScript. The result comes back on the `script_result` signal.
    /// The return value is the id to match that result against.
    #[func]
    fn evaluate_javascript(&mut self, script: GString) -> i64 {
        let id = self.next_script_id;
        self.next_script_id += 1;

        if let Some(inner) = self.inner.as_ref() {
            let sink = inner.sink.clone();
            inner
                .webview
                .evaluate_javascript(script.to_string(), move |result| {
                    let value = result.map(js_value_to_variant).unwrap_or(Variant::nil());
                    sink.push_script_result(id, value);
                });
        }
        id
    }

    /// Change the WebView resolution.
    #[func]
    fn set_view_size_px(&mut self, size: Vector2i) {
        self.view_size = size;
        let size = self.physical_size();
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if let Err(error) = inner.context.recreate_surface(size) {
            godot_error!("godot-servo: could not resize the rendering surface: {error:?}");
            return;
        }
        inner.webview.resize(size);
        // The surface changed, so rebuild the texture bridge too.
        inner.bridge.release();
        inner.bridge = bridge::create(&inner.context, size, &HostContext::capture());
    }

    // ── Input ───────────────────────────────────────────────────────────────

    /// Forward a Godot input event to the WebView.
    ///
    /// `position` is in WebView pixels. For a `TextureRect` that is
    /// `event.position - texture_rect.global_position`; for a 3D panel it is the
    /// UV from the raycast multiplied by the resolution.
    ///
    /// Both mouse and touch events are accepted. Godot's
    /// `input_devices/pointing/emulate_mouse_from_touch`, on by default, also
    /// builds a synthetic mouse event from every touch, so passing both through
    /// unfiltered would deliver one gesture twice. Synthetic events carry
    /// `DEVICE_ID_EMULATION` as their `device` and are dropped here. The same
    /// rule covers `emulate_touch_from_mouse` in the other direction.
    #[func]
    fn feed_input(&mut self, event: Gd<InputEvent>, position: Vector2) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if event.get_device() == DEVICE_ID_EMULATION {
            return;
        }
        let point = DevicePoint::new(position.x, position.y);

        if let Ok(touch) = event.clone().try_cast::<InputEventScreenTouch>() {
            let phase = if touch.is_pressed() {
                TouchEventType::Down
            } else if touch.is_canceled() {
                TouchEventType::Cancel
            } else {
                TouchEventType::Up
            };
            self.feed_touch(phase, touch.get_index(), point);
            return;
        }

        if let Ok(drag) = event.clone().try_cast::<InputEventScreenDrag>() {
            self.feed_touch(TouchEventType::Move, drag.get_index(), point);
            return;
        }

        if let Ok(motion) = event.clone().try_cast::<InputEventMouseMotion>() {
            let _ = motion;
            inner
                .webview
                .notify_input_event(ServoInputEvent::MouseMove(MouseMoveEvent::new(
                    point.into(),
                )));
            return;
        }

        if let Ok(button) = event.clone().try_cast::<InputEventMouseButton>() {
            self.feed_mouse_button(&button, point);
            return;
        }

        if let Ok(key) = event.try_cast::<InputEventKey>() {
            self.feed_key(&key);
        }
    }

    /// Answer `dialog_alert`, `dialog_confirm` or `dialog_prompt`.
    ///
    /// The page's JavaScript stays blocked until this is called, so always call
    /// it somewhere after receiving a dialog signal. A false `accepted` cancels;
    /// `text` is what the `prompt()` field contains and is ignored otherwise.
    #[func]
    fn respond_to_dialog(&mut self, accepted: bool, text: GString) {
        if let Some(inner) = self.inner.as_ref() {
            inner.sink.respond_to_dialog(accepted, &text.to_string());
        }
    }

    /// Answer `select_element_requested`.
    ///
    /// `selected` holds the `id`s of the options the signal handed over. To
    /// cancel instead, call `cancel_pending_dialog()`.
    #[func]
    fn respond_to_select(&mut self, selected: Array<i64>) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let ids = selected
            .iter_shared()
            .filter(|id| *id >= 0)
            .map(|id| id as usize)
            .collect();
        inner.sink.respond_to_select(ids);
    }

    /// Withdraw a pending dialog or `<select>`.
    ///
    /// Sends the default answer, a cancel, and lets the page carry on. Useful as
    /// a safety net when the game closes its own UI.
    #[func]
    fn cancel_pending_dialog(&mut self) {
        if let Some(inner) = self.inner.as_ref() {
            inner.sink.cancel_pending_control();
        }
    }

    /// Whether a dialog or `<select>` is waiting for an answer.
    #[func]
    fn has_pending_dialog(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.sink.has_pending_control())
    }

    /// Tell the WebView the pointer left, so it clears any hover state.
    #[func]
    fn notify_pointer_left(&mut self) {
        if let Some(inner) = self.inner.as_ref() {
            inner
                .webview
                .notify_input_event(ServoInputEvent::MouseMove(MouseMoveEvent::new(
                    DevicePoint::new(-1.0, -1.0).into(),
                )));
        }
    }

    // ── IME ─────────────────────────────────────────────────────────────────

    /// Push a preedit string in directly.
    ///
    /// The entry point for driving composition from the game's own input UI
    /// rather than the OS IME. `state` is `"start"`, `"update"` or `"end"`; the
    /// `text` given with `"end"` is what gets committed.
    #[func]
    fn feed_ime_composition(&mut self, state: GString, text: GString) {
        let state = match state.to_string().as_str() {
            "start" => CompositionState::Start,
            "update" => CompositionState::Update,
            "end" => CompositionState::End,
            other => {
                godot_error!("godot-servo: unknown composition state '{other}'");
                return;
            }
        };
        self.send_composition(state, text.to_string());
    }

    /// Cancel the composition.
    #[func]
    fn cancel_ime_composition(&mut self) {
        if let Some(inner) = self.inner.as_ref() {
            inner
                .webview
                .notify_input_event(ServoInputEvent::Ime(ImeEvent::Dismissed));
        }
        self.composing = false;
        self.last_preedit = GString::new();
    }

    fn send_composition(&self, state: CompositionState, data: String) {
        if let Some(inner) = self.inner.as_ref() {
            inner
                .webview
                .notify_input_event(ServoInputEvent::Ime(ImeEvent::Composition(
                    CompositionEvent { state, data },
                )));
        }
    }

    /// Read the preedit the OS IME holds and pass it to Servo.
    ///
    /// An empty preedit means the composition ended, but `End` must not be sent
    /// here. Servo's `compositionend` assumes `data` carries the committed text;
    /// given an empty one it only clears the selection and leaves the preedit in
    /// place (`handle_compositionend` in `text_input.rs`). Feeding the committed
    /// text in as key events after that would leave both the preedit and the
    /// commit, doubling the input.
    ///
    /// Godot's `ime_get_text()` returns only `GCS_COMPSTR` and never the
    /// committed text, so the commit is assembled from the key events that
    /// follow and carried on `End`. `flush_commit()` does the actual send.
    fn sync_preedit(&mut self) {
        let preedit = DisplayServer::singleton().ime_get_text();
        self.feed_ime_preedit(preedit);
    }

    /// Replace the preedit string.
    ///
    /// `sync_preedit()` calls this for the OS IME. A custom input UI can push
    /// into it directly: pass the preedit, then an empty string, then send the
    /// committed characters through `feed_input()` to follow the same route the
    /// OS IME takes.
    #[func]
    fn feed_ime_preedit(&mut self, preedit: GString) {
        if preedit == self.last_preedit {
            return;
        }
        self.last_preedit = preedit.clone();

        if preedit.is_empty() {
            if self.composing {
                self.composing = false;
                self.awaiting_commit = true;
            }
            return;
        }

        let state = if self.composing {
            CompositionState::Update
        } else {
            self.composing = true;
            CompositionState::Start
        };
        self.send_composition(state, preedit.to_string());
    }

    /// Send the committed text gathered after the composition ended, as `End`.
    ///
    /// Windows delivers "the preedit went empty" first and the `WM_CHAR`s of the
    /// committed text second; Godot hands over the former as a notification and
    /// the latter as key events, within the same frame. By the time `_process`
    /// runs, the committed text is therefore complete.
    ///
    /// A cancelled composition arrives empty. Servo then leaves the preedit in
    /// place rather than removing it, since `clear_selection()` only deselects
    /// as described above, and the composition API offers no way to delete it
    /// from this side, so it stays as it is.
    fn flush_commit(&mut self) {
        if !self.awaiting_commit {
            return;
        }
        self.awaiting_commit = false;
        let commit = std::mem::take(&mut self.pending_commit);
        self.send_composition(CompositionState::End, commit);
    }

    fn set_ime_enabled(&mut self, enabled: bool) {
        let Some(window) = self.base().get_window() else {
            return;
        };
        let window_id = window.get_window_id();
        let mut display_server = DisplayServer::singleton();

        if enabled {
            display_server
                .window_set_ime_position_ex(self.ime_anchor.to_vector2i())
                .window_id(window_id)
                .done();
        }
        display_server
            .window_set_ime_active_ex(enabled)
            .window_id(window_id)
            .done();
        self.ime_active = enabled;

        if !enabled {
            self.composing = false;
            self.awaiting_commit = false;
            self.last_preedit = GString::new();
            self.pending_commit.clear();
        }
    }

    // ── Internals ───────────────────────────────────────────────────────────

    fn physical_size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(
            self.view_size.x.max(1) as u32,
            self.view_size.y.max(1) as u32,
        )
    }

    /// Pass one touch point to Servo.
    ///
    /// Scrolling, flinging and pinching are all handled by Servo's own touch
    /// handler, so all that happens here is relaying the finger going down,
    /// moving, and coming up.
    fn feed_touch(&self, phase: TouchEventType, index: i32, point: DevicePoint) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        inner
            .webview
            .notify_input_event(ServoInputEvent::Touch(TouchEvent::new(
                phase,
                TouchId(index),
                point.into(),
                TouchPointerType::Touch,
            )));
    }

    fn feed_mouse_button(&self, event: &Gd<InputEventMouseButton>, point: DevicePoint) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let index = event.get_button_index();
        let pressed = event.is_pressed();

        // The wheel goes out as a scroll, not as a button.
        let wheel = match index {
            GodotMouseButton::WHEEL_UP => Some((0.0, WHEEL_LINE_HEIGHT)),
            GodotMouseButton::WHEEL_DOWN => Some((0.0, -WHEEL_LINE_HEIGHT)),
            GodotMouseButton::WHEEL_LEFT => Some((WHEEL_LINE_HEIGHT, 0.0)),
            GodotMouseButton::WHEEL_RIGHT => Some((-WHEEL_LINE_HEIGHT, 0.0)),
            _ => None,
        };
        if let Some((x, y)) = wheel {
            if !pressed {
                return;
            }
            let factor = event.get_factor().max(1.0) as f64;
            inner
                .webview
                .notify_input_event(ServoInputEvent::Wheel(WheelEvent::new(
                    WheelDelta {
                        x: x * factor,
                        y: y * factor,
                        z: 0.0,
                        mode: WheelMode::DeltaPixel,
                    },
                    point.into(),
                )));
            return;
        }

        let button = match index {
            GodotMouseButton::LEFT => MouseButton::Left,
            GodotMouseButton::RIGHT => MouseButton::Right,
            GodotMouseButton::MIDDLE => MouseButton::Middle,
            GodotMouseButton::XBUTTON1 => MouseButton::Back,
            GodotMouseButton::XBUTTON2 => MouseButton::Forward,
            _ => return,
        };
        let action = if pressed {
            MouseButtonAction::Down
        } else {
            MouseButtonAction::Up
        };

        // Without the position arriving before the button, Servo sometimes hits
        // the wrong element.
        inner
            .webview
            .notify_input_event(ServoInputEvent::MouseMove(MouseMoveEvent::new(
                point.into(),
            )));
        inner
            .webview
            .notify_input_event(ServoInputEvent::MouseButton(MouseButtonEvent::new(
                action,
                button,
                point.into(),
            )));
    }

    fn feed_key(&mut self, event: &Gd<InputEventKey>) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };

        let state = if event.is_pressed() {
            KeyState::Down
        } else {
            KeyState::Up
        };

        let mut modifiers = Modifiers::empty();
        if event.is_shift_pressed() {
            modifiers |= Modifiers::SHIFT;
        }
        if event.is_ctrl_pressed() {
            modifiers |= Modifiers::CONTROL;
        }
        if event.is_alt_pressed() {
            modifiers |= Modifiers::ALT;
        }
        if event.is_meta_pressed() {
            modifiers |= Modifiers::META;
        }

        let Some(key) = godot_key_to_servo(event) else {
            return;
        };

        // Characters arriving right after a composition are the committed text.
        // Sending them as keys would double the input, so they are only collected
        // here and `flush_commit()` carries them on `End`.
        if self.awaiting_commit {
            if let (true, Key::Character(text)) = (event.is_pressed(), &key) {
                self.pending_commit.push_str(text);
                return;
            }
        }

        inner.webview.notify_input_event(ServoInputEvent::Keyboard(
            KeyboardEvent::new_without_event(
                state,
                key,
                Code::Unidentified,
                Location::Standard,
                modifiers,
                event.is_echo(),
                false,
            ),
        ));
    }

    /// The per-frame work: pump Servo, repaint when needed, emit what queued up.
    fn pump(&mut self) {
        if self.inner.is_none() {
            return;
        }
        // Input handling finished before this frame's `_process`, so the committed
        // text is complete by now.
        self.flush_commit();

        let Some(inner) = self.inner.as_ref() else {
            return;
        };

        // Only for as long as Servo borrows the GL context; Godot's is restored on
        // the way out. `spin_event_loop()` touches GL too, so capture before it.
        let _host_context = HostContext::capture();

        // Servo wakes us from its own threads; do not drop those requests.
        inner.waker.take_pending();
        inner.servo.spin_event_loop();

        let repaint = inner.sink.take_dirty();
        let events = inner.sink.drain();

        if repaint {
            if let Err(error) = inner.context.make_current_public() {
                godot_error!("godot-servo: make_current failed: {error:?}");
                return;
            }
            inner.webview.paint();
        }

        if repaint {
            // Take the frame right after `paint()`, while the FBO still holds it.
            let Some(inner) = self.inner.as_mut() else {
                return;
            };
            let context = inner.context.clone();
            if let Err(error) = inner.bridge.update(&context) {
                godot_error!("godot-servo: texture update failed: {error}");
            }
            servo::RenderingContext::present(context.as_ref());
            self.signals().frame_updated().emit();
        }

        self.emit_events(events);
    }

    fn emit_events(&mut self, events: Vec<ServoEvent>) {
        for event in events {
            match event {
                ServoEvent::UrlChanged(url) => {
                    self.url = GString::from(&url);
                    self.signals().url_changed().emit(&GString::from(&url));
                }
                ServoEvent::TitleChanged(title) => {
                    self.signals().title_changed().emit(&GString::from(&title));
                }
                ServoEvent::LoadStarted => self.signals().load_started().emit(),
                ServoEvent::LoadFinished => self.signals().load_finished().emit(),
                ServoEvent::CursorChanged(cursor) => {
                    let shape = format!("{cursor:?}").to_lowercase();
                    self.signals().cursor_changed().emit(&GString::from(&shape));
                }
                ServoEvent::ConsoleMessage { level, message } => {
                    self.signals()
                        .console_message()
                        .emit(&GString::from(&level), &GString::from(&message));
                }
                ServoEvent::Bridge { name, payload } => {
                    self.signals()
                        .bridge_event()
                        .emit(&GString::from(&name), &GString::from(&payload));
                }
                ServoEvent::ScriptResult { id, value } => {
                    self.signals().script_result().emit(id, &value);
                }
                ServoEvent::ImeShow {
                    x,
                    y,
                    width,
                    height,
                    multiline,
                } => {
                    self.set_ime_enabled(true);
                    let caret = Rect2::new(Vector2::new(x, y), Vector2::new(width, height));
                    self.signals().ime_requested().emit(caret, multiline);
                }
                ServoEvent::ImeHide => {
                    self.set_ime_enabled(false);
                    self.signals().ime_dismissed().emit();
                }
                ServoEvent::Crashed { reason } => {
                    self.signals().crashed().emit(&GString::from(&reason));
                }
                ServoEvent::DialogAlert { message } => {
                    self.signals().dialog_alert().emit(&GString::from(&message));
                }
                ServoEvent::DialogConfirm { message } => {
                    self.signals()
                        .dialog_confirm()
                        .emit(&GString::from(&message));
                }
                ServoEvent::DialogPrompt {
                    message,
                    default_value,
                } => {
                    self.signals()
                        .dialog_prompt()
                        .emit(&GString::from(&message), &GString::from(&default_value));
                }
                ServoEvent::SelectElement {
                    options,
                    allow_multiple,
                } => {
                    let mut array = Array::<Variant>::new();
                    for option in options {
                        let mut entry = Dictionary::<GString, Variant>::new();
                        entry.set(&GString::from("id"), &option.id.to_variant());
                        entry.set(
                            &GString::from("label"),
                            &GString::from(&option.label).to_variant(),
                        );
                        entry.set(&GString::from("disabled"), &option.disabled.to_variant());
                        entry.set(
                            &GString::from("group"),
                            &GString::from(&option.group).to_variant(),
                        );
                        array.push(&entry.to_variant());
                    }
                    self.signals()
                        .select_element_requested()
                        .emit(&array, allow_multiple);
                }
            }
        }
    }
}

fn godot_key_to_servo(event: &Gd<InputEventKey>) -> Option<Key> {
    use servo::NamedKey;

    let named = match event.get_keycode() {
        GodotKey::ENTER | GodotKey::KP_ENTER => NamedKey::Enter,
        GodotKey::BACKSPACE => NamedKey::Backspace,
        GodotKey::TAB => NamedKey::Tab,
        GodotKey::ESCAPE => NamedKey::Escape,
        GodotKey::DELETE => NamedKey::Delete,
        GodotKey::LEFT => NamedKey::ArrowLeft,
        GodotKey::RIGHT => NamedKey::ArrowRight,
        GodotKey::UP => NamedKey::ArrowUp,
        GodotKey::DOWN => NamedKey::ArrowDown,
        GodotKey::HOME => NamedKey::Home,
        GodotKey::END => NamedKey::End,
        GodotKey::PAGEUP => NamedKey::PageUp,
        GodotKey::PAGEDOWN => NamedKey::PageDown,
        GodotKey::SHIFT => NamedKey::Shift,
        GodotKey::CTRL => NamedKey::Control,
        GodotKey::ALT => NamedKey::Alt,
        GodotKey::META => NamedKey::Meta,
        _ => {
            // Printable characters go through unchanged.
            let unicode = event.get_unicode();
            let character = char::from_u32(unicode).filter(|c| !c.is_control())?;
            return Some(Key::Character(character.to_string()));
        }
    };
    Some(Key::Named(named))
}

fn js_value_to_variant(value: JSValue) -> Variant {
    match value {
        JSValue::Undefined | JSValue::Null => Variant::nil(),
        JSValue::Boolean(value) => value.to_variant(),
        JSValue::Number(value) => value.to_variant(),
        JSValue::String(value)
        | JSValue::Element(value)
        | JSValue::ShadowRoot(value)
        | JSValue::Frame(value)
        | JSValue::Window(value) => GString::from(&value).to_variant(),
        JSValue::Array(values) => {
            let mut array = Array::<Variant>::new();
            for value in values {
                array.push(&js_value_to_variant(value));
            }
            array.to_variant()
        }
        JSValue::Object(entries) => {
            let mut dictionary = Dictionary::<GString, Variant>::new();
            for (key, value) in entries {
                dictionary.set(&GString::from(&key), &js_value_to_variant(value));
            }
            dictionary.to_variant()
        }
    }
}

/// One `Servo` per process, shared by every `ServoWebView`.
mod servo_instance {
    use super::*;

    thread_local! {
        static INSTANCE: RefCell<Option<Servo>> = const { RefCell::new(None) };
        static REFCOUNT: RefCell<usize> = const { RefCell::new(0) };
    }

    pub fn acquire(waker: &GodotWaker) -> Servo {
        REFCOUNT.with(|count| *count.borrow_mut() += 1);
        INSTANCE.with(|instance| {
            let mut instance = instance.borrow_mut();
            instance
                .get_or_insert_with(|| {
                    install_crypto_provider();
                    let servo = ServoBuilder::default()
                        .event_loop_waker(Box::new(waker.clone()))
                        .build();
                    servo.setup_logging();
                    servo
                })
                .clone()
        })
    }

    pub fn release() {
        let remaining = REFCOUNT.with(|count| {
            let mut count = count.borrow_mut();
            *count = count.saturating_sub(1);
            *count
        });
        if remaining == 0 {
            INSTANCE.with(|instance| instance.borrow_mut().take());
        }
    }

    /// Servo's network layer assumes a rustls provider has been installed.
    fn install_crypto_provider() {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        }
    }
}
