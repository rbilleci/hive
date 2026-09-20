//! The refresh state machine the project dashboard and the agent overview share: one request in
//! flight, a jittered 15-30 s schedule that backs off on failure, no requests while the tab is hidden
//! or the browser is offline, and a retained snapshot that is dropped the moment the server says the
//! resource is gone, so inaccessible data never lingers.

use crate::graphql::GraphqlError;
use crate::shell::use_console;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

const REFRESH_MINIMUM_DELAY_MS: f64 = 15_000.0;
const REFRESH_MAXIMUM_DELAY_MS: f64 = 30_000.0;
const FAILURE_BACKOFF_STEP_MS: f64 = 5_000.0;

pub type Fetch<K, T> = fn(K) -> Pin<Box<dyn Future<Output = Result<Option<T>, GraphqlError>>>>;

#[derive(Clone, PartialEq)]
pub struct Snapshot<T> {
    pub value: T,
    pub refreshed_at: f64,
}

#[derive(Clone, PartialEq)]
pub enum RefreshState<T> {
    Loading,
    SessionError,
    Unavailable,
    Error,
    Loaded {
        snapshot: Snapshot<T>,
        refreshing: bool,
        refresh_error: bool,
    },
}

pub struct Refreshing<T: Send + Sync + 'static> {
    pub state: RwSignal<RefreshState<T>>,
    /// The loaded snapshot; changes only when the data does, not on every clock tick.
    pub snapshot: Memo<Option<Snapshot<T>>>,
    /// `(refreshing, refresh_error)` of the loaded state.
    pub flags: Memo<(bool, bool)>,
    /// Changes when the state moves between loading, error, and loaded, and not within one of them.
    pub shape: Memo<std::mem::Discriminant<RefreshState<T>>>,
    pub online: RwSignal<bool>,
    /// `Date.now()`, advanced once a second for the freshness line.
    pub clock: RwSignal<f64>,
    pub retry: Callback<()>,
}

impl<T: Send + Sync + 'static> Clone for Refreshing<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Send + Sync + 'static> Copy for Refreshing<T> {}

pub fn iso(value: &str) -> String {
    String::from(js_sys::Date::new(&value.into()).to_iso_string())
}

pub fn iso_millis(value: f64) -> String {
    String::from(js_sys::Date::new(&value.into()).to_iso_string())
}

/// "Last successful <subject> refresh at <time> (<n> seconds ago)."
pub fn freshness_text(subject: &str, refreshed_at: f64, now: f64) -> String {
    let seconds = ((now - refreshed_at) / 1_000.0).floor().max(0.0);
    format!(
        "Last successful {subject} refresh at {} ({seconds} seconds ago).",
        iso_millis(refreshed_at)
    )
}

/// A jittered delay whose lower bound grows with consecutive failures, capped at the maximum.
fn refresh_delay(failures: u32) -> Duration {
    let lower = (REFRESH_MINIMUM_DELAY_MS + f64::from(failures) * FAILURE_BACKOFF_STEP_MS)
        .min(REFRESH_MAXIMUM_DELAY_MS);
    let delay = lower + (js_sys::Math::random() * (REFRESH_MAXIMUM_DELAY_MS - lower + 1.0)).floor();
    Duration::from_millis(delay as u64)
}

fn visible() -> bool {
    document().visibility_state() == web_sys::VisibilityState::Visible
}

/// Keeps `fetch(key)` fresh for the lifetime of the calling component.
pub fn use_refreshing<K, T>(key: Memo<K>, fetch: Fetch<K, T>) -> Refreshing<T>
where
    K: Clone + PartialEq + Send + Sync + 'static,
    T: Clone + PartialEq + Send + Sync + 'static,
{
    let revision = use_console().revision;
    let state = RwSignal::new(RefreshState::<T>::Loading);
    let attempt = RwSignal::new(0_u32);
    let online = RwSignal::new(window().navigator().on_line());
    let clock = RwSignal::new(js_sys::Date::now());
    let generation = StoredValue::new(0_u32);
    let in_flight = StoredValue::new(None::<u32>);
    let failures = StoredValue::new(0_u32);
    let latest = StoredValue::new(None::<Snapshot<T>>);
    let timer = StoredValue::new(None::<TimeoutHandle>);

    let clear_timer = move || {
        if let Some(handle) = timer.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    };
    let is_current = move |request: u32, wanted: &K| {
        generation.try_get_value() == Some(request)
            && key
                .try_get_untracked()
                .is_some_and(|current| current == *wanted)
    };
    let retained = move |refreshing: bool, refresh_error: bool| match latest.get_value() {
        Some(snapshot) => {
            state.set(RefreshState::Loaded {
                snapshot,
                refreshing,
                refresh_error,
            });
            true
        }
        None => false,
    };

    // `refresh` schedules the next refresh, which calls `refresh`; the stored callback ties the knot.
    let refresh = StoredValue::new(None::<Callback<(u32, K)>>);
    let run_refresh = move |request: u32, wanted: K| {
        if let Some(callback) = refresh.try_get_value().flatten() {
            callback.run((request, wanted));
        }
    };
    let schedule = move |request: u32, wanted: K| {
        clear_timer();
        if !is_current(request, &wanted) || !visible() || !online.get_untracked() {
            return;
        }
        timer.set_value(
            set_timeout_with_handle(
                move || run_refresh(request, wanted),
                refresh_delay(failures.get_value()),
            )
            .ok(),
        );
    };
    refresh.set_value(Some(Callback::new(move |(request, wanted): (u32, K)| {
        if !is_current(request, &wanted) || in_flight.get_value() == Some(request) {
            return;
        }
        if !online.get_untracked() {
            retained(false, false);
            return;
        }
        in_flight.set_value(Some(request));
        if !retained(true, false) {
            state.set(RefreshState::Loading);
        }
        spawn_local(async move {
            let result = fetch(wanted.clone()).await;
            if in_flight.try_get_value() == Some(Some(request)) {
                in_flight.set_value(None);
            }
            if !is_current(request, &wanted) {
                return;
            }
            match result {
                Ok(Some(value)) => {
                    let snapshot = Snapshot {
                        value,
                        refreshed_at: js_sys::Date::now(),
                    };
                    latest.set_value(Some(snapshot.clone()));
                    failures.set_value(0);
                    state.set(RefreshState::Loaded {
                        snapshot,
                        refreshing: false,
                        refresh_error: false,
                    });
                }
                Ok(None) => {
                    latest.set_value(None);
                    state.set(RefreshState::Unavailable);
                    return;
                }
                Err(GraphqlError::SessionExpired) => {
                    latest.set_value(None);
                    state.set(RefreshState::SessionError);
                    return;
                }
                Err(GraphqlError::Transport(_)) => {
                    failures.update_value(|count| *count += 1);
                    if !retained(false, true) {
                        state.set(RefreshState::Error);
                    }
                }
            }
            schedule(request, wanted);
        });
    })));

    Effect::new(move |_| {
        let wanted = key.get();
        attempt.track();
        let _ = revision.get();
        clear_timer();
        let request = generation.get_value() + 1;
        generation.set_value(request);
        latest.set_value(None);
        failures.set_value(0);
        in_flight.set_value(None);
        run_refresh(request, wanted);
    });

    let refresh_when_visible = move || {
        if visible() && online.get_untracked() {
            run_refresh(generation.get_value(), key.get_untracked());
        } else {
            clear_timer();
        }
    };
    let on_focus = window_event_listener(ev::focus, move |_| refresh_when_visible());
    crate::dom::on_document_event("visibilitychange", refresh_when_visible);
    let on_online = window_event_listener(ev::online, move |_| {
        online.set(true);
        refresh_when_visible();
    });
    let on_offline = window_event_listener(ev::offline, move |_| {
        online.set(false);
        clear_timer();
        retained(false, false);
    });
    let tick = set_interval_with_handle(
        move || clock.set(js_sys::Date::now()),
        Duration::from_secs(1),
    )
    .ok();
    on_cleanup(move || {
        clear_timer();
        on_focus.remove();
        on_online.remove();
        on_offline.remove();
        if let Some(tick) = tick {
            tick.clear();
        }
    });

    Refreshing {
        state,
        snapshot: Memo::new(move |_| match state.get() {
            RefreshState::Loaded { snapshot, .. } => Some(snapshot),
            _ => None,
        }),
        flags: Memo::new(move |_| match state.get() {
            RefreshState::Loaded {
                refreshing,
                refresh_error,
                ..
            } => (refreshing, refresh_error),
            _ => (false, false),
        }),
        shape: Memo::new(move |_| std::mem::discriminant(&state.get())),
        online,
        clock,
        retry: Callback::new(move |()| attempt.update(|value| *value += 1)),
    }
}

/// The four non-loaded screens, which differ between pages only in their wording and class prefix.
pub struct RefreshCopy {
    pub main_class: &'static str,
    pub css: &'static str,
    pub loading_label: &'static str,
    pub skeleton_label: &'static str,
    pub offline: &'static str,
    pub session: &'static str,
    pub unavailable: &'static str,
    pub error_label: &'static str,
    pub error: &'static str,
}

/// `None` when the state is loaded and the page should render its own content.
pub fn unloaded_view<T: Clone + Send + Sync + 'static>(
    state: &RefreshState<T>,
    copy: &'static RefreshCopy,
    online: RwSignal<bool>,
    retry: Callback<()>,
) -> Option<AnyView> {
    Some(match state {
        RefreshState::Loaded { .. } => return None,
        RefreshState::Loading => view! {
            <main class=copy.main_class aria-label=copy.loading_label>
                {move || if online.get() {
                    view! { <section class=format!("{}-skeleton", copy.css) aria-label=copy.skeleton_label><div></div><div></div><div></div><div></div><div></div><div></div></section> }.into_any()
                } else {
                    view! { <p role="status">{copy.offline}</p> }.into_any()
                }}
            </main>
        }.into_any(),
        RefreshState::SessionError => view! { <main class=copy.main_class><p role="alert">{copy.session}</p></main> }.into_any(),
        RefreshState::Unavailable => view! { <main class=copy.main_class><p role="status">{copy.unavailable}</p></main> }.into_any(),
        RefreshState::Error => view! {
            <main class=copy.main_class><section class=format!("{}-error", copy.css) aria-label=copy.error_label>
                <p role="alert">{copy.error}</p><button type="button" on:click=move |_| retry.run(())>"Retry"</button></section></main>
        }.into_any(),
    })
}
