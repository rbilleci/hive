//! The CodeMirror bridge and the safe Markdown preview.

use leptos::html::Div;
use leptos::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use wasm_bindgen::prelude::*;

// The CodeMirror bundle is a third of the console's download and only source fields need it, so it
// is fetched by a dynamic `import()` the first time an editor mounts. A `module = "…"` binding
// would make it a static import of the wasm-bindgen glue, loaded on every page.
#[wasm_bindgen(
    inline_js = "export function load_hive_editor() { return import('/hive-editor.js'); }"
)]
extern "C" {
    fn load_hive_editor() -> js_sys::Promise;
}

thread_local! {
    static EDITOR_MODULE: std::cell::RefCell<Option<JsValue>> = const { std::cell::RefCell::new(None) };
}

/// The loaded bundle's exports. Concurrent first calls share the browser's single module fetch.
async fn editor_module() -> Result<JsValue, JsValue> {
    if let Some(module) = EDITOR_MODULE.with(|cached| cached.borrow().clone()) {
        return Ok(module);
    }
    let module = wasm_bindgen_futures::JsFuture::from(load_hive_editor()).await?;
    EDITOR_MODULE.with(|cached| *cached.borrow_mut() = Some(module.clone()));
    Ok(module)
}

fn call(module: &JsValue, name: &str, arguments: &[&JsValue]) -> JsValue {
    let function: js_sys::Function = js_sys::Reflect::get(module, &JsValue::from_str(name))
        .expect("the editor bundle exports this function")
        .unchecked_into();
    let list = js_sys::Array::new();
    for argument in arguments {
        list.push(argument);
    }
    function
        .apply(&JsValue::UNDEFINED, &list)
        .unwrap_or(JsValue::UNDEFINED)
}

pub const SUPPORTED_LANGUAGES: [(&str, &str); 8] = [
    ("markdown", "Markdown"),
    ("python", "Python"),
    ("shell", "Shell"),
    ("json", "JSON"),
    ("javascript", "JavaScript"),
    ("typescript", "TypeScript"),
    ("xml", "XML"),
    ("text", "Plain text"),
];

pub fn supported_language(value: Option<&str>, fallback: &'static str) -> String {
    value
        .filter(|candidate| {
            SUPPORTED_LANGUAGES
                .iter()
                .any(|(code, _)| code == candidate)
        })
        .unwrap_or(fallback)
        .to_string()
}

static NEXT_EDITOR_ID: AtomicUsize = AtomicUsize::new(0);

/// Groups digits the way `Number.prototype.toLocaleString("en-US")` does for a non-negative integer.
fn grouped(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

struct Mounted {
    module: JsValue,
    handle: JsValue,
    // Dropping the closure would invalidate the callback CodeMirror still holds.
    _on_change: Closure<dyn FnMut(String)>,
}

/// A small, owned CodeMirror 6 bridge for source fields only; structured fields remain native controls.
#[component]
pub fn CodeEditor(
    label: &'static str,
    #[prop(into)] value: Signal<String>,
    #[prop(into)] language: Signal<String>,
    on_change: Callback<String>,
    /// Without it the header names the language instead of offering a choice.
    #[prop(optional)]
    on_language_change: Option<Callback<String>>,
    #[prop(optional)] focus_id: Option<String>,
    #[prop(optional)] language_select_id: Option<String>,
    /// The id of text outside the editor that also describes it.
    #[prop(optional)]
    described_by: Option<&'static str>,
) -> impl IntoView {
    let id = NEXT_EDITOR_ID.fetch_add(1, Ordering::Relaxed);
    let label_id = format!("source-editor-label-{id}");
    let feedback_id = format!("source-editor-feedback-{id}");
    let host = NodeRef::<Div>::new();
    let mounted = StoredValue::new_local(None::<Mounted>);
    let epoch = StoredValue::new(0_u32);
    let unmount = move || {
        epoch.try_update_value(|current| *current += 1);
        if let Some(previous) = mounted.try_update_value(Option::take).flatten() {
            call(&previous.module, "destroy", &[&previous.handle]);
        }
    };

    // Recreated when the language changes: the language is a fixed extension.
    Effect::new({
        let (label_id, feedback_id, focus_id) =
            (label_id.clone(), feedback_id.clone(), focus_id.clone());
        move |_| {
            let mode = language.get();
            let Some(element) = host.get() else { return };
            unmount();
            // A mount superseded while the bundle loads (a language change, an unmount) is dropped.
            let request = epoch.get_value() + 1;
            epoch.set_value(request);
            let (label_id, feedback_id, focus_id) =
                (label_id.clone(), feedback_id.clone(), focus_id.clone());
            leptos::task::spawn_local(async move {
                let Ok(module) = editor_module().await else {
                    return;
                };
                if epoch.try_get_value() != Some(request) {
                    return;
                }
                let options = js_sys::Object::new();
                for (key, entry) in [
                    ("doc", value.get_untracked()),
                    ("language", mode),
                    ("label", label.to_string()),
                    ("labelledBy", label_id),
                    (
                        "describedBy",
                        described_by.map_or(feedback_id.clone(), |outside| {
                            format!("{outside} {feedback_id}")
                        }),
                    ),
                    ("focusId", focus_id.unwrap_or_default()),
                ] {
                    let _ = js_sys::Reflect::set(
                        &options,
                        &JsValue::from_str(key),
                        &JsValue::from_str(&entry),
                    );
                }
                let callback =
                    Closure::<dyn FnMut(String)>::new(move |text: String| on_change.run(text));
                let handle = call(
                    &module,
                    "create",
                    &[element.as_ref(), &options, callback.as_ref()],
                );
                mounted.set_value(Some(Mounted {
                    module,
                    handle,
                    _on_change: callback,
                }));
            });
        }
    });
    Effect::new(move |_| {
        let text = value.get();
        mounted.with_value(|current| {
            if let Some(current) = current {
                call(
                    &current.module,
                    "setDocument",
                    &[&current.handle, &JsValue::from_str(&text)],
                );
            }
        });
    });
    on_cleanup(unmount);

    let feedback = move || {
        let text = value.get();
        format!(
            "{} characters · {} words · {} bytes. Source content is not sent to analytics.",
            grouped(text.chars().map(char::len_utf16).sum()),
            grouped(text.split_whitespace().count()),
            grouped(text.len())
        )
    };
    view! {
        <div class="source-editor" data-editor-language=move || language.get()>
            <div class="source-editor-header">
                <span id=label_id.clone()>{label}</span>
                {match on_language_change {
                    Some(change) => view! {
                        <label>"Language"
                            <select id=language_select_id aria-label=format!("{label} language")
                                prop:value=move || language.get()
                                on:change=move |event| change.run(event_target_value(&event))>
                                {SUPPORTED_LANGUAGES.iter().map(|(code, name)| view! { <option value=*code selected=move || language.get() == *code>{*name}</option> }).collect_view()}
                            </select>
                        </label>
                    }.into_any(),
                    None => view! { <span>{move || { let current = language.get(); SUPPORTED_LANGUAGES.iter().find(|(code, _)| *code == current).map_or("Plain text", |(_, name)| name) }}</span> }.into_any(),
                }}
            </div>
            <div node_ref=host class="source-editor-host" aria-labelledby=label_id></div>
            <p id=feedback_id class="source-editor-feedback" role="status" aria-live="polite">{feedback}</p>
        </div>
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PreviewBlock {
    Heading(u8, String),
    Paragraph(String),
    Code(String),
}

/// Removes every `<script>…</script>` span, case-insensitively, before any line is read.
fn without_scripts(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0;
    while let Some(start) = lower[cursor..].find("<script").map(|index| index + cursor) {
        let Some(end) = lower[start..]
            .find("</script>")
            .map(|index| index + start + "</script>".len())
        else {
            break;
        };
        out.push_str(&source[cursor..start]);
        cursor = end;
    }
    out.push_str(&source[cursor..]);
    out
}

/// Drops `<…>` tags and reduces `[text](target)` to `text`, so a preview never carries markup or a link.
fn plain_line(line: &str) -> String {
    let mut untagged = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        match rest[open..].find('>') {
            Some(close) => {
                untagged.push_str(&rest[..open]);
                rest = &rest[open + close + 1..];
            }
            None => break,
        }
    }
    untagged.push_str(rest);

    let mut out = String::new();
    let mut rest = untagged.as_str();
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after
            .find("](")
            .filter(|close| *close > 0)
            .and_then(|close| {
                after[close + 2..]
                    .find(')')
                    .map(|end| (close, close + 2 + end + 1))
            });
        match link {
            Some((close, consumed)) if !after[..close].contains(']') => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..close]);
                rest = &after[consumed..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Headings, bullets, paragraphs, and fenced code only.
pub fn preview_blocks(source: &str) -> Vec<PreviewBlock> {
    let mut blocks = Vec::new();
    let mut code: Vec<&str> = Vec::new();
    let mut fenced = false;
    let cleaned = without_scripts(source);
    for raw in cleaned.split('\n') {
        if raw.trim_start().starts_with("```") {
            fenced = !fenced;
            if !fenced && !code.is_empty() {
                blocks.push(PreviewBlock::Code(code.join("\n")));
                code.clear();
            }
            continue;
        }
        if fenced {
            code.push(raw);
            continue;
        }
        let line = plain_line(raw);
        if let Some(text) = line.strip_prefix("### ") {
            blocks.push(PreviewBlock::Heading(4, text.to_string()));
        } else if let Some(text) = line.strip_prefix("## ") {
            blocks.push(PreviewBlock::Heading(3, text.to_string()));
        } else if let Some(text) = line.strip_prefix("# ") {
            blocks.push(PreviewBlock::Heading(2, text.to_string()));
        } else if let Some(text) = line.strip_prefix("- ") {
            blocks.push(PreviewBlock::Paragraph(format!("• {text}")));
        } else if !line.trim().is_empty() {
            blocks.push(PreviewBlock::Paragraph(line));
        }
    }
    if !code.is_empty() {
        blocks.push(PreviewBlock::Code(code.join("\n")));
    }
    blocks
}

/// Preview deliberately accepts only locally parsed text structure and never emits HTML or links.
#[component]
pub fn SafeMarkdownPreview(#[prop(into)] source: Signal<String>) -> impl IntoView {
    view! {
        <section class="markdown-preview" aria-label="Safe Markdown preview">
            {move || {
                let blocks = preview_blocks(&source.get());
                if blocks.is_empty() { return view! { <p>"No preview content."</p> }.into_any(); }
                blocks.into_iter().map(|block| match block {
                    PreviewBlock::Heading(2, text) => view! { <h2>{text}</h2> }.into_any(),
                    PreviewBlock::Heading(3, text) => view! { <h3>{text}</h3> }.into_any(),
                    PreviewBlock::Heading(_, text) => view! { <h4>{text}</h4> }.into_any(),
                    PreviewBlock::Paragraph(text) => view! { <p>{text}</p> }.into_any(),
                    PreviewBlock::Code(text) => view! { <pre><code>{text}</code></pre> }.into_any(),
                }).collect_view().into_any()
            }}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_strips_scripts_tags_and_link_targets() {
        let blocks = preview_blocks("# Title\n<script>alert(1)</script>\nSee [docs](https://example.invalid) <b>now</b>\n- item\n```\nlet x = <y>;\n```");
        assert_eq!(
            blocks,
            vec![
                PreviewBlock::Heading(2, "Title".to_string()),
                PreviewBlock::Paragraph("See docs now".to_string()),
                PreviewBlock::Paragraph("• item".to_string()),
                PreviewBlock::Code("let x = <y>;".to_string()),
            ]
        );
    }

    #[test]
    fn grouped_matches_en_us_digit_grouping() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1234567), "1,234,567");
    }
}
