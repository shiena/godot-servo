//! Godot 側に見えるノード `ServoWebView`。
//!
//! ノード 1 つが Servo の `WebView` 1 枚に対応する。`Servo` 本体はプロセスに 1 つで、
//! 複数のノードで共有する。

use std::cell::RefCell;
use std::rc::Rc;

use dpi::PhysicalSize;
use godot::classes::{INode, InputEvent, InputEventKey, InputEventMouseButton, InputEventMouseMotion, Node, Texture2D};
use godot::global::{Key as GodotKey, MouseButton as GodotMouseButton};
use godot::prelude::*;
use servo::{
    Code, DevicePoint, InputEvent as ServoInputEvent, JSValue, Key, KeyState, KeyboardEvent,
    Location, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, Servo,
    ServoBuilder, UserContentManager, UserScript, WebView, WebViewBuilder, WheelDelta, WheelEvent,
    WheelMode,
};

use crate::bridge::{self, TextureBridge};
use crate::delegate::{ServoEvent, ServoEventSink, BRIDGE_SCRIPT};
use crate::rendering_context::GodotRenderingContext;
use crate::waker::GodotWaker;

/// ホイール 1 ノッチあたりのピクセル数。servoshell の値に合わせている。
const WHEEL_LINE_HEIGHT: f64 = 76.0;

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

    /// 起動時に開く URL。
    #[export]
    #[init(val = GString::from("about:blank"))]
    url: GString,

    /// WebView の解像度 (ピクセル)。
    #[export]
    #[init(val = Vector2i::new(1024, 768))]
    view_size: Vector2i,

    /// `_ready()` で自動的に起動するか。
    #[export]
    #[init(val = true)]
    autostart: bool,

    inner: Option<Inner>,
    next_script_id: i64,
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
}

#[godot_api]
impl ServoWebView {
    // ── シグナル ────────────────────────────────────────────────────────────

    /// テクスチャの中身が更新された。
    #[signal]
    fn frame_updated();

    /// ページのタイトルが変わった。
    #[signal]
    fn title_changed(title: GString);

    /// 表示中の URL が変わった。
    #[signal]
    fn url_changed(url: GString);

    #[signal]
    fn load_started();

    #[signal]
    fn load_finished();

    /// ページが要求しているカーソル形状が変わった。ホバー判定に使える。
    #[signal]
    fn cursor_changed(shape: GString);

    /// `console.log` などの出力。
    #[signal]
    fn console_message(level: GString, message: GString);

    /// ページから Godot に投げられたイベント。
    ///
    /// ページ側では以下のどちらかで発火する。
    ///
    /// ```js
    /// godot.emit("buy", { item: "potion" });   // payload は JSON 文字列で届く
    /// ```
    /// ```html
    /// <a href="godot:buy?item=potion">買う</a>  <!-- payload はクエリ文字列 -->
    /// ```
    #[signal]
    fn bridge_event(name: GString, payload: GString);

    /// `evaluate_javascript()` の結果。`id` は呼び出し時の戻り値と対応する。
    #[signal]
    fn script_result(id: i64, value: Variant);

    // ── 生存管理 ────────────────────────────────────────────────────────────

    #[func]
    fn start(&mut self) {
        if self.inner.is_some() {
            return;
        }
        let size = self.physical_size();

        // surfman が ANGLE を掴む前に、拡張と同じフォルダから読み込ませておく。
        crate::angle_loader::preload();

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

        let bridge = bridge::create(&context, size);
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

    /// 実際に使われているテクスチャ共有経路の名前。
    #[func]
    fn get_backend_name(&self) -> GString {
        match &self.inner {
            Some(inner) => GString::from(inner.bridge.backend_name()),
            None => GString::from("stopped"),
        }
    }

    // ── 表示 ────────────────────────────────────────────────────────────────

    /// Servo の描画結果。マテリアルや `TextureRect` にそのまま挿せる。
    #[func]
    fn get_texture(&self) -> Option<Gd<Texture2D>> {
        self.inner.as_ref().map(|inner| inner.bridge.texture())
    }

    /// テクスチャが上下反転しているか。macOS の共有経路だけ `true` になる。
    #[func]
    fn is_texture_flipped_v(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.bridge.needs_v_flip())
    }

    // ── 操作 ────────────────────────────────────────────────────────────────

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

    /// JavaScript を評価する。結果は `script_result` シグナルで返る。
    /// 戻り値は結果と突き合わせるための id。
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

    /// WebView の解像度を変える。
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
        // サーフェスが変わったので、テクスチャの橋も作り直す。
        inner.bridge.release();
        inner.bridge = bridge::create(&inner.context, size);
    }

    // ── 入力 ────────────────────────────────────────────────────────────────

    /// Godot の入力イベントを WebView に転送する。
    ///
    /// `position` は WebView 内のピクセル座標。`TextureRect` なら
    /// `event.position - texture_rect.global_position`、3D パネルなら
    /// レイキャストで得た UV に解像度を掛けたものを渡す。
    #[func]
    fn feed_input(&mut self, event: Gd<InputEvent>, position: Vector2) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let point = DevicePoint::new(position.x, position.y);

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

    /// ポインタが WebView の外に出たことを伝える。ホバー状態を解除させる。
    #[func]
    fn notify_pointer_left(&mut self) {
        if let Some(inner) = self.inner.as_ref() {
            inner.webview.notify_input_event(ServoInputEvent::MouseMove(
                MouseMoveEvent::new(DevicePoint::new(-1.0, -1.0).into()),
            ));
        }
    }

    // ── 内部 ────────────────────────────────────────────────────────────────

    fn physical_size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(
            self.view_size.x.max(1) as u32,
            self.view_size.y.max(1) as u32,
        )
    }

    fn feed_mouse_button(&self, event: &Gd<InputEventMouseButton>, point: DevicePoint) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let index = event.get_button_index();
        let pressed = event.is_pressed();

        // ホイールはボタンではなくスクロールとして送る。
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

        // ボタンの前に位置を伝えておかないと、Servo 側が違う要素を叩くことがある。
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

    fn feed_key(&self, event: &Gd<InputEventKey>) {
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

        inner
            .webview
            .notify_input_event(ServoInputEvent::Keyboard(KeyboardEvent::new_without_event(
                state,
                key,
                Code::Unidentified,
                Location::Standard,
                modifiers,
                event.is_echo(),
                false,
            )));
    }

    /// 毎フレームの処理。Servo を回し、必要なら描き直し、溜まった通知を emit する。
    fn pump(&mut self) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };

        // Servo は自前のスレッドから起こしてくるので、その要求を取りこぼさない。
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
            // `paint()` の直後、まだ FBO に結果が乗っている状態で取り込む。
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
            // 印字可能な文字はそのまま渡す。
            let unicode = event.get_unicode();
            let character = char::from_u32(unicode as u32).filter(|c| !c.is_control())?;
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

/// `Servo` はプロセスに 1 つ。複数の `ServoWebView` で共有する。
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

    /// Servo のネットワーク層は rustls の provider が入っていることを前提にしている。
    fn install_crypto_provider() {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        }
    }
}
