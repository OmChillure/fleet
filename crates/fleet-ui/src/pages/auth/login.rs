use dioxus::prelude::*;

use super::api::{LoginBody, list_workspaces, login as api_login, take_just_registered};
use super::layout::AuthSplit;
use crate::components::toast::show_toast;
use crate::Route;

/// Login — left form, right product panel. Sets JWT cookies via fleet-auth.
#[component]
pub fn Login() -> Element {
    let mut email = use_signal(|| String::new());
    let mut password = use_signal(|| String::new());
    let mut loading = use_signal(|| false);
    let navigator = use_navigator();

    // Toast after successful signup → login redirect.
    use_effect(move || {
        if take_just_registered() {
            show_toast().success("Account created. Log in with your email and password.");
        }
    });

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        if loading() {
            return;
        }

        let email_val = email.read().trim().to_string();
        let password_val = password.read().clone();

        if email_val.is_empty() || password_val.is_empty() {
            show_toast().error("Enter email and password.");
            return;
        }

        loading.set(true);
        let navigator = navigator.clone();
        spawn(async move {
            match api_login(LoginBody {
                email: email_val,
                password: password_val,
            })
            .await
            {
                Ok(res) => {
                    // Prefer workspace returned by login (created on the spot if none).
                    let mut ws_id = res.default_workspace.map(|w| w.id);
                    if ws_id.is_none() {
                        // Fallback: list (server ensures a default exists).
                        match list_workspaces().await {
                            Ok(list) => ws_id = list.into_iter().next().map(|w| w.id),
                            Err(e) => {
                                loading.set(false);
                                show_toast().error(e.message());
                                return;
                            }
                        }
                    }
                    loading.set(false);
                    if let Some(id) = ws_id {
                        show_toast().success("Signed in.");
                        navigator.push(Route::Dashboard {
                            workspace_id: id,
                        });
                    } else {
                        show_toast().error("Logged in, but no workspace was found. Try again.");
                    }
                }
                Err(e) => {
                    loading.set(false);
                    show_toast().error(e.message());
                }
            }
        });
    };

    let form = rsx! {
        h1 { class: "mb-1.5 text-2xl font-bold tracking-tight text-white", "Welcome back" }
        p { class: "mb-6 text-[0.95rem] text-neutral-400",
            "Sign in to manage your agent fleet."
        }

        form { onsubmit: on_submit, class: "space-y-4",
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
                    placeholder: "••••••••",
                    autocomplete: "current-password",
                    value: "{password}",
                    oninput: move |e| password.set(e.value()),
                }
            }

            button {
                class: "inline-flex w-full items-center justify-center rounded-lg bg-white px-4 py-2.5 text-[0.92rem] font-semibold text-black transition hover:bg-neutral-200 disabled:cursor-not-allowed disabled:opacity-60",
                r#type: "submit",
                disabled: loading(),
                if loading() { "Signing in…" } else { "Log in" }
            }
        }

        p { class: "mt-6 text-center text-sm text-neutral-500",
            "No account yet? "
            Link { class: "font-semibold text-white underline-offset-2 hover:underline", to: Route::Signup {},
                "Create one"
            }
        }
    };

    rsx! {
        AuthSplit {
            form,
            right_title: "Pick up where the fleet left off".to_string(),
            right_subtitle: "Sessions, sandboxes, and approvals stay durable — sign in and supervise work that outlives any single agent.".to_string(),
        }
    }
}
