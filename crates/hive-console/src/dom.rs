//! Small browser helpers shared by the shell and the pages.

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// The elements matching `selector` inside `container`, in document order, skipping hidden ones.
pub fn elements(container: &web_sys::Element, selector: &str) -> Vec<web_sys::HtmlElement> {
    let Ok(nodes) = container.query_selector_all(selector) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.item(index)?.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|element| !element.has_attribute("hidden"))
        .collect()
}

pub fn active_element() -> Option<web_sys::Element> {
    document().active_element()
}

fn position(targets: &[web_sys::HtmlElement]) -> Option<usize> {
    let active = active_element()?;
    targets
        .iter()
        .position(|target| AsRef::<web_sys::Element>::as_ref(target) == &active)
}

/// Modal keyboard containment for the lifetime of the calling effect: focuses the first match of
/// `selector`, keeps Tab and Shift+Tab inside `container`, and calls `on_escape` for Escape.
pub fn contain_focus(
    container: impl Fn() -> Option<web_sys::Element> + Clone + 'static,
    selector: &'static str,
    on_escape: impl Fn() + 'static,
) {
    // Focus moves as soon as the panel exists, so a key pressed right after it opens lands inside
    // it; the frame callback covers a panel that is not mounted yet.
    let first = container.clone();
    let focus_first = move || {
        first()
            .and_then(|panel| elements(&panel, selector).into_iter().next())
            .is_some_and(|target| target.focus().is_ok())
    };
    if !focus_first() {
        request_animation_frame(move || {
            focus_first();
        });
    }
    let trap = window_event_listener(ev::keydown, move |event| {
        if event.key() == "Escape" {
            event.prevent_default();
            on_escape();
            return;
        }
        if event.key() != "Tab" {
            return;
        }
        let Some(panel) = container() else { return };
        let targets = elements(&panel, selector);
        let (Some(first), Some(last)) = (targets.first(), targets.last()) else {
            event.prevent_default();
            return;
        };
        let index = position(&targets);
        if event.shift_key() && index.is_none_or(|index| index == 0) {
            event.prevent_default();
            let _ = last.focus();
        } else if !event.shift_key() && index == Some(targets.len() - 1) {
            event.prevent_default();
            let _ = first.focus();
        }
    });
    on_cleanup(move || trap.remove());
}

/// Moves focus among `items` for the roving keys of a menu. Returns whether the key was handled.
pub fn move_menu_focus(items: &[web_sys::HtmlElement], key: &str) -> bool {
    if items.is_empty() {
        return false;
    }
    let current = position(items).unwrap_or(0);
    let target = match key {
        "ArrowDown" => (current + 1) % items.len(),
        "ArrowUp" => (current + items.len() - 1) % items.len(),
        "Home" => 0,
        "End" => items.len() - 1,
        _ => return false,
    };
    let _ = items[target].focus();
    true
}

pub fn local_storage() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}

pub fn session_storage() -> Option<web_sys::Storage> {
    window().session_storage().ok().flatten()
}

/// A listener on `document` that lives until the calling owner is cleaned up. `visibilitychange`
/// is dispatched on the document and does not bubble, so a window listener never sees it.
pub fn on_document_event(name: &'static str, handler: impl Fn() + 'static) {
    use wasm_bindgen::closure::Closure;
    let callback = Closure::<dyn Fn()>::new(handler);
    let _ = document().add_event_listener_with_callback(name, callback.as_ref().unchecked_ref());
    let registered = StoredValue::new_local(Some(callback));
    on_cleanup(move || {
        if let Some(callback) = registered.try_update_value(Option::take).flatten() {
            let _ = document()
                .remove_event_listener_with_callback(name, callback.as_ref().unchecked_ref());
        }
    });
}
