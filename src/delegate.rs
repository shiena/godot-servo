//! Servo からの通知を受け取り、キューに積む。
//!
//! シグナルをここから直接 emit しないのは再入を避けるため。通知は
//! `Servo::spin_event_loop()` の内側で飛んでくるので、その場でノードを
//! `bind_mut()` すると借用が衝突する。いったんキューに積み、`_process` の中で
//! 安全に取り出して emit する。

use std::cell::{Cell, RefCell};

use godot::prelude::Variant;
use servo::{
    ConsoleLogLevel, Cursor, EmbedderControl, LoadStatus, NavigationRequest, WebView,
    WebViewDelegate,
};
use url::Url;

/// `window.godot.emit()` が `console.log` に流し込むときの目印。
pub const CONSOLE_BRIDGE_PREFIX: &str = "\u{1}godot-servo\u{1}";

/// HTML から直接叩けるスキーム。`<a href="godot:buy?item=potion">` のように書く。
pub const BRIDGE_SCHEME: &str = "godot";

#[derive(Debug)]
pub enum ServoEvent {
    UrlChanged(String),
    TitleChanged(String),
    LoadStarted,
    LoadFinished,
    CursorChanged(Cursor),
    ConsoleMessage {
        level: String,
        message: String,
    },
    /// ページ側から Godot に投げられたイベント。
    Bridge {
        name: String,
        payload: String,
    },
    /// `evaluate_javascript()` の結果。
    ScriptResult {
        id: i64,
        value: Variant,
    },
    /// 編集可能な要素にフォーカスが入った。IME を起こす。
    /// 座標は WebView 内のピクセルで、キャレットの矩形。
    ImeShow {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        multiline: bool,
    },
    /// フォーカスが外れた。IME を落とす。
    ImeHide,
}

#[derive(Default)]
pub struct ServoEventSink {
    dirty: Cell<bool>,
    queue: RefCell<Vec<ServoEvent>>,
}

impl ServoEventSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// 描き直しが必要かを読んで下ろす。
    pub fn take_dirty(&self) -> bool {
        self.dirty.replace(false)
    }

    pub fn mark_dirty(&self) {
        self.dirty.set(true);
    }

    pub fn drain(&self) -> Vec<ServoEvent> {
        std::mem::take(&mut *self.queue.borrow_mut())
    }

    pub fn push_script_result(&self, id: i64, value: Variant) {
        self.push(ServoEvent::ScriptResult { id, value });
    }

    fn push(&self, event: ServoEvent) {
        self.queue.borrow_mut().push(event);
    }
}

impl WebViewDelegate for ServoEventSink {
    fn notify_new_frame_ready(&self, _webview: WebView) {
        self.dirty.set(true);
    }

    fn notify_url_changed(&self, _webview: WebView, url: Url) {
        self.push(ServoEvent::UrlChanged(url.to_string()));
    }

    fn notify_page_title_changed(&self, _webview: WebView, title: Option<String>) {
        self.push(ServoEvent::TitleChanged(title.unwrap_or_default()));
    }

    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        match status {
            LoadStatus::Started => self.push(ServoEvent::LoadStarted),
            LoadStatus::Complete => self.push(ServoEvent::LoadFinished),
            _ => {}
        }
    }

    fn notify_cursor_changed(&self, _webview: WebView, cursor: Cursor) {
        self.push(ServoEvent::CursorChanged(cursor));
    }

    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        // `window.godot.emit()` が目印つきで流してくるものは橋として扱う。
        if let Some(body) = message.strip_prefix(CONSOLE_BRIDGE_PREFIX) {
            let (name, payload) = body.split_once('\u{1}').unwrap_or((body, ""));
            self.push(ServoEvent::Bridge {
                name: name.to_owned(),
                payload: payload.to_owned(),
            });
            return;
        }
        self.push(ServoEvent::ConsoleMessage {
            level: format!("{level:?}").to_lowercase(),
            message,
        });
    }

    /// 編集可能な要素にフォーカスが入ると Servo がこれを寄こす。
    ///
    /// IME 以外の種類 (`<select>` のピッカーやダイアログ) はここで drop している。
    /// drop すると Servo 側で「キャンセルされた」扱いになるので、扱えないものを
    /// 黙って握り潰すより素直な既定になる。
    fn show_embedder_control(&self, _webview: WebView, embedder_control: EmbedderControl) {
        if let EmbedderControl::InputMethod(control) = embedder_control {
            let rect = control.position();
            self.push(ServoEvent::ImeShow {
                x: rect.min.x as f32,
                y: rect.min.y as f32,
                width: rect.width() as f32,
                height: rect.height() as f32,
                multiline: control.multiline(),
            });
        }
    }

    fn hide_embedder_control(&self, _webview: WebView, _control_id: servo::EmbedderControlId) {
        self.push(ServoEvent::ImeHide);
    }

    /// `godot:` スキームへの遷移を横取りしてイベントに変換する。
    ///
    /// JavaScript を書かずに、素の `<a href="godot:...">` だけで Godot 側に
    /// 通知を飛ばせるようにするための経路。
    fn request_navigation(&self, _webview: WebView, navigation_request: NavigationRequest) {
        if navigation_request.url.scheme() != BRIDGE_SCHEME {
            navigation_request.allow();
            return;
        }

        let url = &navigation_request.url;
        let name = url.path().trim_start_matches('/').to_owned();
        let payload = url.query().unwrap_or_default().to_owned();
        self.push(ServoEvent::Bridge { name, payload });
        // 実際にはページを移動させない。
        navigation_request.deny();
    }
}

/// ページに注入する橋渡し用スクリプト。
///
/// ```js
/// godot.emit("buy", { item: "potion" });
/// ```
///
/// 中身は目印つきの `console.log` で、`show_console_message` が拾う。ナビゲーションを
/// 発生させないので、ページの状態には一切影響しない。
pub const BRIDGE_SCRIPT: &str = concat!(
    "(function(){",
    "if (window.godot) { return; }",
    "var MARK = '\\u0001godot-servo\\u0001';",
    "window.godot = {",
    "  emit: function(name, payload) {",
    "    var body;",
    "    try { body = JSON.stringify(payload === undefined ? null : payload); }",
    "    catch (error) { body = JSON.stringify(String(payload)); }",
    "    console.log(MARK + String(name) + '\\u0001' + body);",
    "  }",
    "};",
    "})();",
);
