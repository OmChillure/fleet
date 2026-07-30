use dioxus::prelude::*;

use super::api::{SignupBody, mark_just_registered, signup as api_signup};
use super::layout::AuthSplit;
use crate::Route;

/// Signup — left form, right panel. On success → `/login?registered=1`.
#[component]
pub fn Signup() -> Element {
    let mut display_name = use_signal(|| String::new());
    let mut email = use_signal(|| String::new());
    let mut password = use_signal(|| String::new());
    let mut error = use_signal(|| Option::<String>::None);
    let mut loading = use_signal(|| false);
    let navigator = use_navigator();

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        if loading() {
            return;
        }
        error.set(None);

        let email_val = email.read().trim().to_string();
        let password_val = password.read().clone();
        let name_val = display_name.read().trim().to_string();

        if email_val.is_empty() || password_val.is_empty() {
            error.set(Some("Email and password are required.".into()));
            return;
        }
        if password_val.len() < 8 {
            error.set(Some("Password must be at least 8 characters.".into()));
            return;
        }

        loading.set(true);
        let navigator = navigator.clone();
        spawn(async move {
            match api_signup(SignupBody {
                email: email_val,
                password: password_val,
                display_name: if name_val.is_empty() {
                    None
                } else {
                    Some(name_val)
                },
            })
            .await
            {
                Ok(_) => {
                    loading.set(false);
                    // Product flow: after signup → login (no auto session).
                    mark_just_registered();
                    navigator.push(Route::Login {});
                }
                Err(e) => {
                    loading.set(false);
                    error.set(Some(e.message()));
                }
            }
        });
    };

    let form = rsx! {
        h1 { class: "mb-1.5 text-2xl font-bold tracking-tight text-white", "Create your account" }
        p { class: "mb-6 text-[0.95rem] text-neutral-400",
            "Spin up a workspace and start orchestrating agents."
        }

        form { onsubmit: on_submit, class: "space-y-4",
            div { class: "flex flex-col gap-1.5",
                label { class: "text-sm font-semibold text-neutral-400", r#for: "display_name", "Display name" }
                input {
                    class: "rounded-lg border border-neutral-800 bg-black px-3.5 py-2.5 text-[0.95rem] text-white outline-none transition focus:border-neutral-500",
                    id: "display_name",
                    r#type: "text",
                    name: "display_name",
                    placeholder: "Ada Lovelace",
                    autocomplete: "name",
                    value: "{display_name}",
                    oninput: move |e| display_name.set(e.value()),
                }
            }
            div { class: "flex flex-col gap-1.5",
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
            div { class: "flex flex-col gap-1.5",
                label { class: "text-sm font-semibold text-neutral-400", r#for: "password", "Password" }
                input {
                    class: "rounded-lg border border-neutral-800 bg-black px-3.5 py-2.5 text-[0.95rem] text-white outline-none transition focus:border-neutral-500",
                    id: "password",
                    r#type: "password",
                    name: "password",
                    placeholder: "At least 8 characters",
                    autocomplete: "new-password",
                    value: "{password}",
                    oninput: move |e| password.set(e.value()),
                }
            }

            if let Some(msg) = error() {
                p { class: "text-sm text-red-400", "{msg}" }
            }

            button {
                class: "inline-flex w-full items-center justify-center rounded-lg bg-white px-4 py-2.5 text-[0.92rem] font-semibold text-black transition hover:bg-neutral-200 disabled:cursor-not-allowed disabled:opacity-60",
                r#type: "submit",
                disabled: loading(),
                if loading() { "Creating account…" } else { "Sign up" }
            }
        }

        p { class: "mt-6 text-center text-sm text-neutral-500",
            "Already have an account? "
            Link { class: "font-semibold text-white underline-offset-2 hover:underline", to: Route::Login {},
                "Log in"
            }
        }
    };

    rsx! {
        AuthSplit {
            form,
            right_title: "Stand up a fleet in minutes".to_string(),
            right_subtitle: "One account, a personal workspace, and a control plane ready for durable multi-agent workflows.".to_string(),
        }
    }
}
