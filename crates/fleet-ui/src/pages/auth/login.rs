use dioxus::prelude::*;

use crate::Route;

/// Placeholder login — accepts any non-empty email/password and routes to dashboard.
#[component]
pub fn Login() -> Element {
    let mut email = use_signal(|| String::new());
    let mut password = use_signal(|| String::new());
    let mut error = use_signal(|| Option::<String>::None);
    let navigator = use_navigator();

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        error.set(None);

        let email_val = email.read().trim().to_string();
        let password_val = password.read().clone();

        if email_val.is_empty() || password_val.is_empty() {
            error.set(Some("Enter email and password.".into()));
            return;
        }

        // Demo auth: any credentials work. Wire to real API via super::api later.
        navigator.push(Route::Dashboard {});
    };

    rsx! {
        div { class: "grid min-h-[calc(100vh-4rem)] place-items-center bg-black px-4 py-8 text-white",
            div { class: "w-full max-w-[420px] rounded-2xl border border-neutral-800 bg-neutral-950 p-7",
                h1 { class: "mb-1.5 text-2xl font-bold tracking-tight text-white", "Welcome back" }
                p { class: "mb-6 text-[0.95rem] text-neutral-400",
                    "Sign in to manage your agent fleet."
                }

                form { onsubmit: on_submit,
                    div { class: "mb-4 flex flex-col gap-1.5",
                        label { class: "text-sm font-semibold text-neutral-400", r#for: "email", "Email" }
                        input {
                            class: "rounded-lg border border-neutral-800 bg-black px-3.5 py-2.5 text-[0.95rem] text-white outline-none transition focus:border-neutral-500",
                            id: "email",
                            r#type: "email",
                            name: "email",
                            placeholder: "you@company.com",
                            autocomplete: "username",
                            value: "{email}",
                            oninput: move |e| email.set(e.value()),
                        }
                    }
                    div { class: "mb-4 flex flex-col gap-1.5",
                        label { class: "text-sm font-semibold text-neutral-400", r#for: "password", "Password" }
                        input {
                            class: "rounded-lg border border-neutral-800 bg-black px-3.5 py-2.5 text-[0.95rem] text-white outline-none transition focus:border-neutral-500",
                            id: "password",
                            r#type: "password",
                            name: "password",
                            placeholder: "••••••••",
                            autocomplete: "current-password",
                            value: "{password}",
                            oninput: move |e| password.set(e.value()),
                        }
                    }

                    if let Some(msg) = error() {
                        p { class: "mb-3 text-sm text-neutral-300", "{msg}" }
                    }

                    button {
                        class: "inline-flex w-full items-center justify-center rounded-lg bg-white px-4 py-2.5 text-[0.92rem] font-semibold text-black transition hover:bg-neutral-200",
                        r#type: "submit",
                        "Log in"
                    }
                }

                p { class: "mt-4 text-center text-[0.82rem] text-neutral-500",
                    "Demo mode — any email and password will open the dashboard."
                }
            }
        }
    }
}
