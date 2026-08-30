//! Receives Servo's notifications and queues them.
//!
//! Signals are not emitted from here, to avoid re-entering the node. The
//! notifications arrive from inside `Servo::spin_event_loop()`, so calling
//! `bind_mut()` on the node at that point would collide with the borrow already
//! held. They go on a queue instead, and `_process` drains it safely.

use std::cell::{Cell, RefCell};

use godot::prelude::Variant;
use servo::{
    AlertDialog, ConfirmDialog, ConsoleLogLevel, Cursor, EmbedderControl, EmbedderControlId,
    LoadStatus, NavigationRequest, PromptDialog, SelectElement, SelectElementOptionOrOptgroup,
    SimpleDialog, WebView, WebViewDelegate,
};
use url::Url;

/// The marker `window.godot.emit()` puts in front of what it sends to `console.log`.
pub const CONSOLE_BRIDGE_PREFIX: &str = "\u{1}godot-servo\u{1}";

/// The scheme HTML can trigger directly, as in `<a href="godot:buy?item=potion">`.
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
    /// An event the page threw at Godot.
    Bridge {
        name: String,
        payload: String,
    },
    /// The result of an `evaluate_javascript()` call.
    ScriptResult {
        id: i64,
        value: Variant,
    },
    /// An editable element took focus; bring the IME up. The coordinates are
    /// the caret rectangle, in WebView pixels.
    ImeShow {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        multiline: bool,
    },
    /// Focus left; take the IME down.
    ImeHide,
    /// The page's process died.
    Crashed {
        reason: String,
    },
    /// `alert()`. Answered with `respond_to_dialog()`.
    DialogAlert {
        message: String,
    },
    /// `confirm()`.
    DialogConfirm {
        message: String,
    },
    /// `prompt()`.
    DialogPrompt {
        message: String,
        default_value: String,
    },
    /// A `<select>` was opened. Answered with `respond_to_select()`.
    SelectElement {
        options: Vec<SelectOption>,
        allow_multiple: bool,
    },
}

/// One option of a `<select>`.
///
/// `<optgroup>`s are flattened into a single list, with the group's name in
/// `group`. `id` is the running number Servo assigns, and goes straight back to
/// `respond_to_select()`.
#[derive(Debug)]
pub struct SelectOption {
    pub id: i64,
    pub label: String,
    pub disabled: bool,
    pub group: String,
}

/// A UI request from Servo that is waiting for an answer.
///
/// Dropping any of these sends the default answer: alert closes, confirm and
/// prompt cancel, select leaves the selection alone. The page's JavaScript is
/// blocked until that answer arrives, so none of these should be held on to.
enum PendingControl {
    Alert(AlertDialog),
    Confirm(ConfirmDialog),
    Prompt(PromptDialog),
    Select(SelectElement),
}

/// A pending request together with its id.
///
/// The id comes from `EmbedderControl::id()` and has to be read before the enum
/// is destructured, because Servo does not expose the id on the individual
/// dialog types.
struct Pending {
    id: EmbedderControlId,
    control: PendingControl,
}

#[derive(Default)]
pub struct ServoEventSink {
    dirty: Cell<bool>,
    queue: RefCell<Vec<ServoEvent>>,
    /// The UI request waiting for an answer. At most one at a time.
    pending: RefCell<Option<Pending>>,
    /// The id of the IME currently shown, so `hide_embedder_control` can tell
    /// the IME apart from everything else.
    ime_control: Cell<Option<EmbedderControlId>>,
}

impl ServoEventSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the repaint flag and clear it.
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

    /// Replace the pending request. The previous one is dropped, which sends its
    /// default answer.
    fn set_pending(&self, id: EmbedderControlId, control: PendingControl) {
        *self.pending.borrow_mut() = Some(Pending { id, control });
    }

    /// Whether anything is waiting for an answer.
    pub fn has_pending_control(&self) -> bool {
        self.pending.borrow().is_some()
    }

    /// Answer an `alert()`, `confirm()` or `prompt()`.
    ///
    /// A false `accepted` cancels. `text` is what the `prompt()` field contains
    /// and is ignored otherwise. Does nothing when nothing is pending.
    pub fn respond_to_dialog(&self, accepted: bool, text: &str) {
        let Some(pending) = self.pending.borrow_mut().take() else {
            return;
        };
        match pending.control {
            PendingControl::Alert(dialog) => dialog.confirm(),
            PendingControl::Confirm(dialog) => {
                if accepted {
                    dialog.confirm()
                } else {
                    dialog.dismiss()
                }
            }
            PendingControl::Prompt(mut dialog) => {
                if accepted {
                    dialog.set_current_value(text);
                    dialog.confirm()
                } else {
                    dialog.dismiss()
                }
            }
            // Not a dialog after all. Dropping it without putting it back sends
            // the default answer.
            PendingControl::Select(_) => {}
        }
    }

    /// Answer a `<select>`. `selected` holds the `id`s of the chosen options.
    pub fn respond_to_select(&self, selected: Vec<usize>) {
        let Some(pending) = self.pending.borrow_mut().take() else {
            return;
        };
        if let PendingControl::Select(mut select) = pending.control {
            select.select(selected);
            select.submit();
        }
    }

    /// Withdraw whatever is pending. Dropping it sends the default answer and
    /// lets the page carry on.
    pub fn cancel_pending_control(&self) {
        *self.pending.borrow_mut() = None;
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
        // What `window.godot.emit()` sends carries a marker; treat it as a bridge
        // event rather than a console message.
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

    /// Servo sends this whenever it wants the embedder to show a control.
    fn show_embedder_control(&self, _webview: WebView, embedder_control: EmbedderControl) {
        // Read the id first: destructuring puts it out of reach.
        let id = embedder_control.id();

        match embedder_control {
            EmbedderControl::InputMethod(control) => {
                let rect = control.position();
                self.ime_control.set(Some(id));
                self.push(ServoEvent::ImeShow {
                    x: rect.min.x as f32,
                    y: rect.min.y as f32,
                    width: rect.width() as f32,
                    height: rect.height() as f32,
                    multiline: control.multiline(),
                });
            }
            EmbedderControl::SimpleDialog(dialog) => match dialog {
                SimpleDialog::Alert(alert) => {
                    let message = alert.message().to_owned();
                    self.set_pending(id, PendingControl::Alert(alert));
                    self.push(ServoEvent::DialogAlert { message });
                }
                SimpleDialog::Confirm(confirm) => {
                    let message = confirm.message().to_owned();
                    self.set_pending(id, PendingControl::Confirm(confirm));
                    self.push(ServoEvent::DialogConfirm { message });
                }
                SimpleDialog::Prompt(prompt) => {
                    let message = prompt.message().to_owned();
                    let default_value = prompt.current_value().to_owned();
                    self.set_pending(id, PendingControl::Prompt(prompt));
                    self.push(ServoEvent::DialogPrompt {
                        message,
                        default_value,
                    });
                }
            },
            EmbedderControl::SelectElement(select) => {
                let options = flatten_options(select.options());
                let allow_multiple = select.allow_select_multiple();
                self.set_pending(id, PendingControl::Select(select));
                self.push(ServoEvent::SelectElement {
                    options,
                    allow_multiple,
                });
            }
            // File picker, color picker and context menu are not handled.
            // Dropping them here sends the default answer, choosing nothing, and
            // the page carries on.
            EmbedderControl::FilePicker(_)
            | EmbedderControl::ColorPicker(_)
            | EmbedderControl::ContextMenu(_) => {}
        }
    }

    /// Servo withdrew a request it had asked for.
    fn hide_embedder_control(&self, _webview: WebView, control_id: servo::EmbedderControlId) {
        if self.ime_control.get() == Some(control_id) {
            self.ime_control.set(None);
            self.push(ServoEvent::ImeHide);
            return;
        }
        // Something that was waiting for an answer got withdrawn. Drop it, which
        // sends the default answer.
        let mut pending = self.pending.borrow_mut();
        if pending.as_ref().is_some_and(|p| p.id == control_id) {
            *pending = None;
        }
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, _backtrace: Option<String>) {
        self.push(ServoEvent::Crashed { reason });
    }

    /// Intercept navigation to the `godot:` scheme and turn it into an event.
    ///
    /// This is what lets a plain `<a href="godot:...">` reach Godot with no
    /// JavaScript at all.
    fn request_navigation(&self, _webview: WebView, navigation_request: NavigationRequest) {
        if navigation_request.url.scheme() != BRIDGE_SCHEME {
            navigation_request.allow();
            return;
        }

        let url = &navigation_request.url;
        let name = url.path().trim_start_matches('/').to_owned();
        let payload = url.query().unwrap_or_default().to_owned();
        self.push(ServoEvent::Bridge { name, payload });
        // Never actually navigate.
        navigation_request.deny();
    }
}

/// The bridge script injected into every page.
///
/// ```js
/// godot.emit("buy", { item: "potion" });
/// ```
///
/// Underneath it is a marked `console.log` that `show_console_message` picks up.
/// It triggers no navigation, so it leaves page state completely alone.
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

/// Flatten `<optgroup>`s into a single list of options.
fn flatten_options(options: &[SelectElementOptionOrOptgroup]) -> Vec<SelectOption> {
    let mut flat = Vec::new();
    for entry in options {
        match entry {
            SelectElementOptionOrOptgroup::Option(option) => flat.push(SelectOption {
                id: option.id as i64,
                label: option.label.clone(),
                disabled: option.is_disabled,
                group: String::new(),
            }),
            SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                for option in options {
                    flat.push(SelectOption {
                        id: option.id as i64,
                        label: option.label.clone(),
                        disabled: option.is_disabled,
                        group: label.clone(),
                    });
                }
            }
        }
    }
    flat
}
