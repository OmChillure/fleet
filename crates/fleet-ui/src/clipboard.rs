//! Copy-to-clipboard for the browser.
//!
//! `navigator.clipboard` exists only in a [secure context] (HTTPS, or
//! `localhost`). Firefox on `http://192.168.x.x` has no clipboard getter —
//! calling it panics, and a silent return looks like a dead Copy button.
//! Fall back to a hidden textarea + `document.execCommand("copy")` in the
//! click gesture.

/// Returns whether the text was placed on the clipboard.
pub fn copy_text(text: &str) -> bool {
    if !fleet_runners_view::clipboard_has_payload(text) {
        return false;
    }
    #[cfg(target_arch = "wasm32")]
    {
        if copy_via_textarea(text) {
            return true;
        }
        copy_via_clipboard_api(text)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
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
    let clip = js_sys::Reflect::get(
        window.navigator().as_ref(),
        &wasm_bindgen::JsValue::from_str("clipboard"),
    )
    .ok()
    .filter(|v| !v.is_undefined() && !v.is_null())
    .and_then(|v| v.dyn_into::<web_sys::Clipboard>().ok());
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
