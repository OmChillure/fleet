//! Copy-to-clipboard for the browser.
//!
//! `navigator.clipboard` exists only in a [secure context] (HTTPS, or
//! `localhost`). Firefox on `http://192.168.x.x` has no clipboard getter —
//! calling it panics, and a silent return looks like a dead Copy button.
//! Fall back to a hidden textarea + `document.execCommand("copy")` in the
//! click gesture.

/// Copy backends in the order `copy_text` tries them.
///
/// Textarea/`execCommand` is first so HTTP LAN (no secure context) never
/// reads `navigator.clipboard`. The clipboard API is only used if that
/// fallback fails and a Reflect probe says the object exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyBackend {
    ExecCommandTextarea,
    ClipboardApiIfPresent,
}

const HTTP_LAN_THEN_API: &[CopyBackend] = &[
    CopyBackend::ExecCommandTextarea,
    CopyBackend::ClipboardApiIfPresent,
];

/// Ordered backends for `text`. Empty payload → none (fail closed).
fn copy_backends(text: &str) -> &'static [CopyBackend] {
    if fleet_runners_view::clipboard_has_payload(text) {
        HTTP_LAN_THEN_API
    } else {
        &[]
    }
}

/// Whether `Reflect.get(navigator, "clipboard")` produced a usable object.
///
/// Missing, undefined, null, or a non-Clipboard value is false. Do not
/// read `navigator.clipboard` directly — that getter panics on Firefox
/// HTTP LAN.
fn clipboard_api_usable(
    reflect_ok: bool,
    is_undefined: bool,
    is_null: bool,
    is_clipboard: bool,
) -> bool {
    reflect_ok && !is_undefined && !is_null && is_clipboard
}

/// Returns whether the text was placed on the clipboard.
pub fn copy_text(text: &str) -> bool {
    let backends = copy_backends(text);
    if backends.is_empty() {
        return false;
    }
    #[cfg(target_arch = "wasm32")]
    {
        for backend in backends {
            match *backend {
                CopyBackend::ExecCommandTextarea => {
                    if copy_via_textarea(text) {
                        return true;
                    }
                }
                CopyBackend::ClipboardApiIfPresent => {
                    if copy_via_clipboard_api(text) {
                        return true;
                    }
                }
            }
        }
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = backends;
        false
    }
}

#[cfg(target_arch = "wasm32")]
fn copy_via_textarea(text: &str) -> bool {
    use wasm_bindgen::JsCast;
    use web_sys::{HtmlDocument, HtmlTextAreaElement};

    let Some(window) = web_sys::window() else {
        return false;
    };
    let Some(document) = window.document() else {
        return false;
    };
    let Ok(el) = document.create_element("textarea") else {
        return false;
    };
    let Ok(ta) = el.dyn_into::<HtmlTextAreaElement>() else {
        return false;
    };
    ta.set_value(text);
    let _ = ta.set_attribute("readonly", "");
    let _ = ta.set_attribute(
        "style",
        "position:fixed;left:-9999px;top:0;width:1px;height:1px;opacity:0",
    );
    let Some(body) = document.body() else {
        return false;
    };
    if body.append_child(&ta).is_err() {
        return false;
    }
    let _ = ta.focus();
    ta.select();
    let _ = ta.set_selection_range(0, text.len() as u32);
    let ok = document
        .dyn_ref::<HtmlDocument>()
        .and_then(|d| d.exec_command("copy").ok())
        .unwrap_or(false);
    let _ = ta.remove();
    ok
}

#[cfg(target_arch = "wasm32")]
fn copy_via_clipboard_api(text: &str) -> bool {
    use wasm_bindgen::JsCast;

    let Some(window) = web_sys::window() else {
        return false;
    };
    let got = js_sys::Reflect::get(
        window.navigator().as_ref(),
        &wasm_bindgen::JsValue::from_str("clipboard"),
    );
    let (undefined, null, clip) = match &got {
        Ok(v) => (
            v.is_undefined(),
            v.is_null(),
            v.clone().dyn_into::<web_sys::Clipboard>().ok(),
        ),
        Err(_) => (true, true, None),
    };
    if !clipboard_api_usable(got.is_ok(), undefined, null, clip.is_some()) {
        return false;
    }
    let Some(clip) = clip else {
        return false;
    };
    let text = text.to_string();
    spawn_clipboard_write(clip, text);
    true
}

#[cfg(target_arch = "wasm32")]
fn spawn_clipboard_write(clip: web_sys::Clipboard, text: String) {
    dioxus::prelude::spawn(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(clip.write_text(&text)).await;
    });
}
