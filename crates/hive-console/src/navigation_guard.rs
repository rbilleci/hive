//! A navigation blocker, which Leptos's router does not provide: while `active`, an in-app link
//! click or a Back/Forward step is held until the user decides.

use leptos::prelude::*;
use wasm_bindgen::prelude::*;

#[derive(Clone, PartialEq)]
enum Pending {
    Link(String),
    History,
}

#[derive(Clone, Copy)]
pub struct NavigationGuard {
    pending: RwSignal<Option<Pending>>,
    bypass: StoredValue<bool>,
}

impl NavigationGuard {
    pub fn blocked(&self) -> bool {
        self.pending.with(Option::is_some)
    }

    pub fn reset(&self) {
        self.pending.set(None);
    }

    /// Carries out the held navigation.
    pub fn proceed(&self, navigate: impl Fn(&str, leptos_router::NavigateOptions)) {
        match self.pending.get_untracked() {
            Some(Pending::Link(href)) => {
                self.pending.set(None);
                navigate(&href, Default::default());
            }
            Some(Pending::History) => {
                self.pending.set(None);
                self.bypass.set_value(true);
                let _ = window().history().and_then(|history| history.back());
            }
            None => {}
        }
    }
}

struct HistoryHooks {
    is_active: Box<dyn Fn() -> bool>,
    hold: Box<dyn Fn()>,
    bypass: StoredValue<bool>,
}

thread_local! {
    static ACTIVE_GUARD: std::cell::RefCell<Option<HistoryHooks>> = const { std::cell::RefCell::new(None) };
    static RESTORING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Registers the one `popstate` listener that can hold a Back or Forward step. Call it before the
/// application mounts: for an event fired at `window` itself, listeners run in registration order
/// whatever their capture flag, and the router's listener unmounts the guarded page, so a listener
/// added by that page would already be gone by the time its turn came.
pub fn install() {
    let callback = Closure::<dyn Fn(web_sys::Event)>::new(move |event: web_sys::Event| {
        if RESTORING.with(|flag| flag.replace(false)) {
            event.stop_immediate_propagation();
            return;
        }
        ACTIVE_GUARD.with(|slot| {
            let slot = slot.borrow();
            let Some(hooks) = slot.as_ref() else { return };
            if hooks.bypass.try_get_value().unwrap_or(false) {
                hooks.bypass.set_value(false);
                return;
            }
            if !(hooks.is_active)() {
                return;
            }
            // The URL has already moved. Step forward to the guarded entry again and hide both steps
            // from the router, so the page and its unsaved state never unmount.
            event.stop_immediate_propagation();
            RESTORING.with(|flag| flag.set(true));
            let _ = window()
                .history()
                .and_then(|history| history.go_with_delta(1));
            (hooks.hold)();
        });
    });
    let _ =
        window().add_event_listener_with_callback("popstate", callback.as_ref().unchecked_ref());
    callback.forget();
}

fn listen(
    target: &web_sys::EventTarget,
    name: &'static str,
    handler: impl Fn(web_sys::Event) + 'static,
) {
    let callback = Closure::<dyn Fn(web_sys::Event)>::new(handler);
    // Capture phase: this must run, and be able to stop the event, before the router's own listener.
    let _ = target.add_event_listener_with_callback_and_bool(
        name,
        callback.as_ref().unchecked_ref(),
        true,
    );
    let registered = StoredValue::new_local(Some((target.clone(), callback)));
    on_cleanup(move || {
        if let Some((target, callback)) = registered.try_update_value(Option::take).flatten() {
            let _ = target.remove_event_listener_with_callback_and_bool(
                name,
                callback.as_ref().unchecked_ref(),
                true,
            );
        }
    });
}

pub fn use_navigation_guard(active: Signal<bool>) -> NavigationGuard {
    let pending = RwSignal::new(None::<Pending>);
    let bypass = StoredValue::new(false);
    let is_active = move || active.try_get_untracked().unwrap_or(false);

    listen(document().as_ref(), "click", move |event| {
        let Some(mouse) = event.dyn_ref::<web_sys::MouseEvent>() else {
            return;
        };
        if !is_active()
            || mouse.button() != 0
            || mouse.meta_key()
            || mouse.ctrl_key()
            || mouse.shift_key()
            || mouse.alt_key()
        {
            return;
        }
        let anchor = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest("a[href]").ok().flatten())
            .and_then(|element| element.dyn_into::<web_sys::HtmlAnchorElement>().ok());
        let Some(anchor) = anchor else { return };
        let location = window().location();
        if anchor.target() == "_blank"
            || anchor.has_attribute("download")
            || Some(anchor.origin()) != location.origin().ok()
        {
            return;
        }
        let destination = format!("{}{}", anchor.pathname(), anchor.search());
        let here = format!(
            "{}{}",
            location.pathname().unwrap_or_default(),
            location.search().unwrap_or_default()
        );
        if destination == here {
            return;
        }
        event.prevent_default();
        event.stop_propagation();
        pending.set(Some(Pending::Link(destination)));
    });

    // The history half is handled by the listener `install` registered before the router's own.
    ACTIVE_GUARD.with(|slot| {
        *slot.borrow_mut() = Some(HistoryHooks {
            is_active: Box::new(is_active),
            hold: Box::new(move || pending.set(Some(Pending::History))),
            bypass,
        })
    });
    on_cleanup(|| ACTIVE_GUARD.with(|slot| *slot.borrow_mut() = None));

    NavigationGuard { pending, bypass }
}
