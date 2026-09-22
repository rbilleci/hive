//! The title block every page shares, with the narrow-viewport drawer trigger.

use crate::shell::use_console;
use leptos::prelude::*;

#[component]
pub fn PageHeader(
    title_id: &'static str,
    #[prop(into)] title: Signal<String>,
    #[prop(optional, into)] description: Option<&'static str>,
    /// A link that closes the description: its target and its text.
    #[prop(optional)]
    description_link: Option<(String, &'static str)>,
    /// Text that follows the link, when the link sits inside the sentence.
    #[prop(optional)]
    description_tail: Option<&'static str>,
    #[prop(optional, into)] meta: Option<Signal<String>>,
    /// A status shown in the meta line behind a decorative marker, so its text stands alone.
    #[prop(optional, into)]
    status: Option<Signal<String>>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let console = use_console();
    view! {
        <header class="page-header">
            <button node_ref=console.drawer_trigger class="console-drawer-trigger" type="button" aria-label="Open navigation"
                aria-haspopup="dialog" aria-controls="console-drawer-title" on:click=move |_| console.drawer_open.set(true)>
                <span aria-hidden="true">"☰"</span><span>"Menu"</span>
            </button>
            <div class="page-header-text">
                <h1 id=title_id>{move || title.get()}</h1>
                {description.map(|text| view! { <p class="page-header-description">{text}{description_link.map(|(href, label)| view! { " "<a href=href>{label}</a> })}{description_tail}</p> })}
                {meta.map(|text| view! { <p class="page-header-meta">{move || text.get()}</p> })}
                {status.map(|text| view! { <p class="page-header-meta"><span class="status-with-text"><span aria-hidden="true">"●"</span><span>{move || text.get()}</span></span></p> })}
            </div>
            {children.map(|children| view! { <div class="page-header-action">{children()}</div> })}
        </header>
    }
}
