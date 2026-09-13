//! Global toasts — always high-contrast (not theme CSS vars; host sits outside AppShell).
//!
//! ```rust
//! show_toast().info("This is a toast!");
//! show_toast().success("Saved.");
//! show_toast().error("Something failed.");
//! ```

use std::cell::Cell;

use dioxus::prelude::*;

const TOAST_MS: u32 = 4000;
const MAX_TOASTS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Clone, PartialEq)]
pub struct ToastItem {
    pub id: u64,
    pub kind: ToastKind,
    pub message: String,
}

#[derive(Clone, Copy)]
pub struct Toaster {
    items: Signal<Vec<ToastItem>>,
    next_id: Signal<u64>,
}

thread_local! {
    static GLOBAL_TOASTER: Cell<Option<Toaster>> = const { Cell::new(None) };
}

pub fn show_toast() -> Toaster {
    GLOBAL_TOASTER
        .with(|c| c.get())
        .expect("show_toast() used before <ToastProvider> mounted")
}

pub fn use_toaster() -> Toaster {
    use_context::<Toaster>()
}

impl Toaster {
    pub fn info(self, message: impl Into<String>) {
        self.push(ToastKind::Info, message.into());
    }

    pub fn success(self, message: impl Into<String>) {
        self.push(ToastKind::Success, message.into());
    }

    pub fn warn(self, message: impl Into<String>) {
        self.push(ToastKind::Warn, message.into());
    }

    pub fn error(self, message: impl Into<String>) {
        self.push(ToastKind::Error, message.into());
    }

    pub fn dismiss(self, id: u64) {
        let mut items = self.items;
        let mut list = items();
        list.retain(|t| t.id != id);
        items.set(list);
    }

    fn push(self, kind: ToastKind, message: String) {
        let message = message.trim().to_string();
        if message.is_empty() {
            return;
        }

        let mut items = self.items;
        let mut next_id = self.next_id;

        let id = next_id();
        next_id.set(id.wrapping_add(1));

        let mut list = items();
        list = merge_toast(
            list,
            ToastItem {
                id,
                kind,
                message,
            },
        );
        items.set(list);

        spawn(async move {
            sleep_ms(TOAST_MS).await;
            let mut list = items();
            list.retain(|t| t.id != id);
            items.set(list);
        });
    }
}

#[component]
pub fn ToastProvider(children: Element) -> Element {
    let items = use_signal(|| Vec::<ToastItem>::new());
    let next_id = use_signal(|| 1u64);
    let toaster = Toaster { items, next_id };
    use_context_provider(|| toaster);
    GLOBAL_TOASTER.with(|c| c.set(Some(toaster)));

    rsx! {
        {children}
        ToastHost {}
    }
}

#[component]
fn ToastHost() -> Element {
    let toaster = use_toaster();
    let items = toaster.items;

    rsx! {
        div {
            // Solid colors only — ToastHost is outside AppShell, so --app-* vars are unset.
            class: "pointer-events-none fixed bottom-3 right-3 z-[9999] flex w-[min(100vw-1.5rem,24rem)] flex-col-reverse gap-3",
            "aria-live": "polite",
            for t in items() {
                {
                    let id = t.id;
                    let (icon_tone, bar) = match t.kind {
                        ToastKind::Info => ("text-zinc-300", "bg-zinc-400"),
                        ToastKind::Success => ("text-emerald-400", "bg-emerald-500"),
                        ToastKind::Warn => ("text-amber-400", "bg-amber-500"),
                        ToastKind::Error => ("text-red-400", "bg-red-500"),
                    };
                    rsx! {
                        div {
                            key: "{id}",
                            // Card stays visible but does not eat clicks (Apply in
                            // the runner side panel sits under this stack).
                            class: "pointer-events-none relative overflow-hidden rounded-none border border-zinc-600 bg-zinc-900 text-zinc-50 shadow-[0_12px_40px_rgba(0,0,0,0.55)]",
                            role: "status",
                            div { class: "flex items-center gap-3 px-3.5 py-3",
                                span {
                                    class: "flex h-8 w-8 shrink-0 items-center justify-center {icon_tone}",
                                    "aria-hidden": "true",
                                    ToastIcon { kind: t.kind }
                                }
                                p {
                                    class: "m-0 min-w-0 flex-1 text-sm leading-snug text-zinc-50",
                                    "{t.message}"
                                }
                                button {
                                    class: "pointer-events-auto shrink-0 rounded-none bg-zinc-100 px-2.5 py-1 text-xs font-semibold text-zinc-900 transition hover:bg-white",
                                    r#type: "button",
                                    "aria-label": "Dismiss",
                                    onclick: move |_| toaster.dismiss(id),
                                    "OK"
                                }
                            }
                            div { class: "h-0.5 w-full bg-zinc-700",
                                div { class: "toast-progress h-full origin-left {bar}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ToastIcon(kind: ToastKind) -> Element {
    match kind {
        ToastKind::Info => rsx! {
            svg {
                class: "h-5 w-5",
                view_box: "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "1.75",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                path { d: "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" }
                path { d: "M10.3 21a1.94 1.94 0 0 0 3.4 0" }
            }
        },
        ToastKind::Success => rsx! {
            svg {
                class: "h-5 w-5",
                view_box: "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "2",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                circle { cx: "12", cy: "12", r: "10" }
                path { d: "m9 12 2 2 4-4" }
            }
        },
        ToastKind::Warn => rsx! {
            svg {
                class: "h-5 w-5",
                view_box: "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "2",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                path { d: "m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3" }
                path { d: "M12 9v4M12 17h.01" }
            }
        },
        ToastKind::Error => rsx! {
            svg {
                class: "h-5 w-5",
                view_box: "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "2",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                circle { cx: "12", cy: "12", r: "10" }
                path { d: "m15 9-6 6M9 9l6 6" }
            }
        },
    }
}

fn merge_toast(mut list: Vec<ToastItem>, item: ToastItem) -> Vec<ToastItem> {
    // One success at a time so create/observe/cancel do not stack over the form.
    if item.kind == ToastKind::Success {
        list.retain(|t| t.kind != ToastKind::Success);
    }
    list.push(item);
    while list.len() > MAX_TOASTS {
        list.remove(0);
    }
    list
}

async fn sleep_ms(ms: u32) {
    #[cfg(target_arch = "wasm32")]
    {
        gloo_timers::future::TimeoutFuture::new(ms).await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, kind: ToastKind, message: &str) -> ToastItem {
        ToastItem {
            id,
            kind,
            message: message.into(),
        }
    }

    #[test]
    fn merge_replaces_previous_success() {
        let list = vec![item(1, ToastKind::Success, "Case created.")];
        let out = merge_toast(list, item(2, ToastKind::Success, "Case cancelled."));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].message, "Case cancelled.");
    }

    #[test]
    fn toast_card_does_not_intercept_page_clicks() {
        let src = include_str!("toast.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap();
        assert!(
            !prod.contains("pointer-events-auto relative overflow-hidden"),
            "toast card must not be pointer-events-auto — it covered Apply"
        );
        assert!(
            prod.contains("pointer-events-none relative overflow-hidden"),
            "toast card must let clicks through to controls underneath"
        );
        assert!(
            prod.contains("pointer-events-auto shrink-0"),
            "only the dismiss button is clickable"
        );
    }

    #[test]
    fn merge_keeps_error_when_success_arrives() {
        let list = vec![item(1, ToastKind::Error, "boom")];
        let out = merge_toast(list, item(2, ToastKind::Success, "ok"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, ToastKind::Error);
        assert_eq!(out[1].kind, ToastKind::Success);
    }
}
