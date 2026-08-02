//! Minimal page shown in the OAuth popup after authorize.
//! Signals the original integrations tab via localStorage and closes itself.
//! Does not load the full app shell.

use dioxus::prelude::*;

#[component]
pub fn OAuthDone() -> Element {
    let mut status = use_signal(|| "Finishing…".to_string());
    let mut ok = use_signal(|| true);

    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        {
            let (connected, err) = parse_query();
            if let Some(window) = web_sys::window() {
                // 1) postMessage opener (primary) so Integrations updates immediately
                notify_opener(&window, connected.as_deref(), err.as_deref());

                // 2) localStorage fallback when opener is missing / blocked
                if let Some(storage) = window.local_storage().ok().flatten() {
                    if let Some(p) = &connected {
                        let _ = storage.set_item("fleet_oauth_result", &format!("connected:{p}"));
                        status.set(format!("{p} connected"));
                        ok.set(true);
                    } else if let Some(e) = &err {
                        let _ = storage.set_item("fleet_oauth_result", &format!("error:{e}"));
                        status.set(format!("Could not connect: {e}"));
                        ok.set(false);
                    } else {
                        status.set("Done. You can close this tab.".into());
                    }
                }

                // Close popup so the user is left on the original Agent Fleet tab.
                let has_opener = window
                    .opener()
                    .ok()
                    .filter(|v| !v.is_null() && !v.is_undefined())
                    .is_some();
                if has_opener {
                    let _ = window.close();
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            status.set("OAuth complete.".into());
        }
    });

    rsx! {
        div {
            class: "flex min-h-screen flex-col items-center justify-center gap-3 bg-black px-6 text-center text-white",
            if ok() {
                div {
                    class: "flex h-12 w-12 items-center justify-center rounded-full bg-emerald-500/15 text-emerald-400",
                    svg {
                        class: "h-6 w-6",
                        view_box: "0 0 24 24",
                        fill: "none",
                        stroke: "currentColor",
                        stroke_width: "2",
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        path { d: "M20 6L9 17l-5-5" }
                    }
                }
            } else {
                div {
                    class: "flex h-12 w-12 items-center justify-center rounded-full bg-red-500/15 text-red-400",
                    span { class: "text-lg font-bold", "!" }
                }
            }
            p { class: "m-0 text-base font-medium", "{status}" }
            p { class: "m-0 max-w-sm text-sm text-neutral-500",
                "Go back to your Agent Fleet tab. This window should close on its own."
            }
            button {
                class: "mt-2 rounded-lg border border-neutral-700 px-4 py-2 text-sm text-neutral-300 transition hover:bg-neutral-900",
                r#type: "button",
                onclick: move |_| {
                    #[cfg(target_arch = "wasm32")]
                    if let Some(w) = web_sys::window() {
                        let _ = w.close();
                    }
                },
                "Close tab"
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn notify_opener(window: &web_sys::Window, connected: Option<&str>, err: Option<&str>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;

    let Ok(opener) = window.opener() else {
        return;
    };
    if opener.is_null() || opener.is_undefined() {
        return;
    }
    let Ok(opener_win) = opener.dyn_into::<web_sys::Window>() else {
        return;
    };

    let payload = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &payload,
        &JsValue::from_str("source"),
        &JsValue::from_str("fleet-oauth"),
    );
    let ok = connected.is_some() && err.is_none();
    let _ = js_sys::Reflect::set(&payload, &JsValue::from_str("ok"), &JsValue::from_bool(ok));
    let _ = js_sys::Reflect::set(
        &payload,
        &JsValue::from_str("provider"),
        &JsValue::from_str(connected.unwrap_or("")),
    );
    let _ = js_sys::Reflect::set(
        &payload,
        &JsValue::from_str("error"),
        &JsValue::from_str(err.unwrap_or("")),
    );

    // Match any origin for local dev (3030 UI / 8080 API).
    let _ = opener_win.post_message(&payload, "*");
    let _ = opener_win.focus();
}

#[cfg(target_arch = "wasm32")]
fn parse_query() -> (Option<String>, Option<String>) {
    let mut connected = None;
    let mut err = None;
    if let Some(window) = web_sys::window() {
        if let Ok(search) = window.location().search() {
            let q = search.trim_start_matches('?');
            for pair in q.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k == "connected" && !v.is_empty() {
                        connected = Some(url_decode(v));
                    } else if k == "error" && !v.is_empty() {
                        err = Some(url_decode(v));
                    }
                }
            }
        }
    }
    (connected, err)
}

#[cfg(target_arch = "wasm32")]
fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v as char);
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}
