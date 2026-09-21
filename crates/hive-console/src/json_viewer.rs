//! A structured, collapsed-by-default JSON viewer with copy and a raw view.
//! Native `<details>` keeps every node collapsed by default and keyboard-operable with no per-node state.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::time::Duration;

#[component]
pub fn JsonViewer(value: Value, label: &'static str) -> impl IntoView {
    let copied = RwSignal::new(false);
    let raw_view = RwSignal::new(false);
    let raw = serde_json::to_string_pretty(&value).unwrap_or_default();
    let copy = {
        let raw = raw.clone();
        move |_| {
            let text = raw.clone();
            spawn_local(async move {
                let written = wasm_bindgen_futures::JsFuture::from(
                    window().navigator().clipboard().write_text(&text),
                )
                .await
                .is_ok();
                copied.set(written);
                if written {
                    set_timeout(
                        move || {
                            let _ = copied.try_set(false);
                        },
                        Duration::from_secs(2),
                    );
                }
            });
        }
    };
    view! {
        <div class="json-viewer" aria-label=label>
            <div class="json-viewer-toolbar">
                <button type="button" on:click=move |_| raw_view.update(|raw| *raw = !*raw)>{move || if raw_view.get() { "Structured view" } else { "Raw view" }}</button>
                <button type="button" on:click=copy>{move || if copied.get() { "Copied" } else { "Copy JSON" }}</button>
            </div>
            {move || if raw_view.get() { view! { <pre class="json-viewer-raw">{raw.clone()}</pre> }.into_any() } else { json_node(&value, label.to_string(), 0) }}
        </div>
    }
}

fn json_node(value: &Value, key: String, depth: usize) -> AnyView {
    let leaf = |text: String| {
        view! { <div class="json-viewer-leaf"><span class="json-viewer-key">{key.clone()}</span>": "<span class="json-viewer-value">{text}</span></div> }.into_any()
    };
    let (entries, count): (Vec<(String, &Value)>, String) = match value {
        Value::Array(items) if !items.is_empty() => (
            items
                .iter()
                .enumerate()
                .map(|(index, item)| (index.to_string(), item))
                .collect(),
            format!("[{}]", items.len()),
        ),
        Value::Object(fields) if !fields.is_empty() => (
            fields
                .iter()
                .map(|(name, item)| (name.clone(), item))
                .collect(),
            format!("{{{}}}", fields.len()),
        ),
        Value::Array(_) => return leaf("[]".to_string()),
        Value::Object(_) => return leaf("{}".to_string()),
        scalar => return leaf(scalar.to_string()),
    };
    view! {
        <details class="json-viewer-node" open=depth == 0>
            <summary>{key.clone()}" "<span class="json-viewer-count">{count}</span></summary>
            <div class="json-viewer-children">{entries.into_iter().map(|(name, item)| json_node(item, name, depth + 1)).collect_view()}</div>
        </details>
    }.into_any()
}
