//! Ports `PreferencesPage.tsx`. Current-principal ergonomics only: non-secret, mutable, and outside
//! authorization and audit decisions.

use crate::api::console::{save_display_preferences, DisplayPreferences};
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::prelude::*;
use leptos::task::spawn_local;

#[component]
pub fn PreferencesPage() -> impl IntoView {
    let shell = use_console().preferences;
    let draft = RwSignal::new(shell.get_untracked());
    let saved = RwSignal::new(shell.get_untracked());
    let saving = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);

    // A change previews in the shell at once; only Save stores it.
    let change = move |apply: fn(&mut DisplayPreferences, String), value: String| {
        if saving.get_untracked() {
            return;
        }
        draft.update(|next| apply(next, value));
        shell.set(draft.get_untracked());
        problem.set(None);
    };
    let save = move || {
        if saving.get_untracked() {
            return;
        }
        saving.set(true);
        problem.set(None);
        let pending = draft.get_untracked();
        spawn_local(async move {
            match save_display_preferences(&pending).await {
                Ok(result) => {
                    draft.set(result.clone());
                    saved.set(result.clone());
                    shell.set(result);
                }
                Err(message) => {
                    let restored = saved.get_untracked();
                    draft.set(restored.clone());
                    shell.set(restored);
                    problem.set(Some(message));
                }
            }
            saving.set(false);
        });
    };

    view! {
        <main class="directory preferences-page" aria-labelledby="preferences-title">
            <PageHeader title_id="preferences-title" title="Display preferences".to_string()
                description="Only display choices are stored here. They are non-secret, update in place for your current account, and never change what you are authorized to do." />
            {move || problem.get().map(|message| view! { <p role="alert">{message}" Your last saved display choices were restored."</p> })}
            <fieldset disabled=move || saving.get()>
                <legend>"Console display"</legend>
                <label>"Color scheme"
                    <select aria-label="Color scheme" prop:value=move || draft.with(|value| value.color_scheme.clone())
                        on:change=move |event| change(|next, value| next.color_scheme = value, event_target_value(&event))>
                        <option value="LIGHT">"Light"</option><option value="DARK">"Dark"</option><option value="SYSTEM">"Use system setting"</option>
                    </select>
                </label>
                <label>"Density"
                    <select aria-label="Density" prop:value=move || draft.with(|value| value.density.clone())
                        on:change=move |event| change(|next, value| next.density = value, event_target_value(&event))>
                        <option value="COMFORTABLE">"Comfortable"</option><option value="COMPACT">"Compact"</option>
                    </select>
                </label>
            </fieldset>
            <button class="primary-action" type="button" on:click=move |_| save() disabled=move || saving.get()>{move || if saving.get() { "Saving…" } else { "Save" }}</button>
            <p class="preferences-save-state" role="status">"Preview changes apply to this shell immediately."</p>
        </main>
    }
}
