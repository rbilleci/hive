//! Ports `ConfirmationDialog.tsx`: a bounded modal with keyboard containment that returns focus to
//! whatever had it when the dialog opened.

use crate::dom::{active_element, contain_focus};
use leptos::html::Div;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

#[component]
pub fn ConfirmationDialog(
    title: &'static str,
    on_close: Callback<()>,
    children: Children,
) -> impl IntoView {
    let dialog = NodeRef::<Div>::new();
    let previous = StoredValue::new_local(
        active_element().and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok()),
    );
    Effect::new(move |_| {
        contain_focus(
            move || dialog.get_untracked().map(Into::into),
            "input,button:not([disabled]),textarea,[tabindex]:not([tabindex='-1'])",
            move || on_close.run(()),
        );
    });
    // Focus returns to the control that opened the dialog. When the confirmed action re-rendered
    // that control, the same-named button that replaced it takes focus instead.
    on_cleanup(move || {
        let Some(element) = previous.try_get_value().flatten() else {
            return;
        };
        let _ = element.focus();
        request_animation_frame(move || {
            if element.is_connected() {
                return;
            }
            let Some(main) = document().query_selector("main").ok().flatten() else {
                return;
            };
            let label = element.text_content();
            if let Some(replacement) = crate::dom::elements(&main, "button")
                .into_iter()
                .find(|button| button.text_content() == label)
            {
                let _ = replacement.focus();
            }
        });
    });
    view! {
        <div class="confirmation-backdrop" role="presentation">
            <div node_ref=dialog class="confirmation-dialog" role="dialog" aria-modal="true" aria-labelledby="confirmation-title">
                <h2 id="confirmation-title">{title}</h2>{children()}
            </div>
        </div>
    }
}
