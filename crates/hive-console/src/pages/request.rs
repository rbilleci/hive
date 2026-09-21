//! The one request/state machine every page loads through: one request in flight per page, a
//! response discarded unless it belongs to the key and the generation that asked for it, a retained
//! snapshot dropped the moment the server says the resource is gone, and the four unloaded screens
//! (loading, expired session, unavailable, failed) rendered from one place.
//!
//! What differs between pages is a `Policy`, not a second copy of this machinery: whether the page
//! polls and how often, whether a new key keeps the rows already on screen, and whether a change to
//! the console's own capabilities reloads it.

use crate::format::iso_millis;
use crate::graphql::GraphqlError;
use crate::shell::use_console;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// The polling schedule the dashboard and the agent overview share.
pub const SLOW_POLL: Cadence = Cadence::Polling {
    minimum: Duration::from_secs(15),
    maximum: Duration::from_secs(30),
    backoff_step: Duration::from_secs(5),
};

pub type Fetch<K, T> = fn(K) -> Pin<Box<dyn Future<Output = Result<Option<T>, GraphqlError>>>>;

/// How often a loaded page asks again by itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    /// Never: the key, the console's capabilities, or the page's own control asks.
    OnDemand,
    /// After a jittered delay between `minimum` and `maximum`, whose lower bound grows by
    /// `backoff_step` per consecutive failure up to `maximum`. Nothing is requested while the tab
    /// is hidden or the browser is offline.
    Polling {
        minimum: Duration,
        maximum: Duration,
        backoff_step: Duration,
    },
}

/// What a page shows while the response for a new key is in flight.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// The loading screen: the previous key described a different subject.
    Fresh,
    /// The rows already on screen, marked as refreshing, so a filter control the reader is typing
    /// into stays mounted and keeps its focus.
    Retain,
}

#[derive(Clone, Copy)]
pub struct Policy {
    pub cadence: Cadence,
    pub retention: Retention,
    /// Reload when the console's capability revision changes, which follows a membership or
    /// lifecycle change. A page that holds unsaved edits from its own response sets this false.
    pub follow_revision: bool,
}

impl Policy {
    pub fn on_demand() -> Self {
        Self {
            cadence: Cadence::OnDemand,
            retention: Retention::Fresh,
            follow_revision: true,
        }
    }

    pub fn polling(cadence: Cadence) -> Self {
        Self {
            cadence,
            ..Self::on_demand()
        }
    }

    pub fn retaining(self) -> Self {
        Self {
            retention: Retention::Retain,
            ..self
        }
    }

    pub fn ignoring_revision(self) -> Self {
        Self {
            follow_revision: false,
            ..self
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Snapshot<T> {
    pub value: T,
    pub refreshed_at: f64,
}

#[derive(Clone, PartialEq)]
pub enum RequestState<T> {
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

pub struct Request<T: Send + Sync + 'static> {
    pub state: RwSignal<RequestState<T>>,
    /// The loaded snapshot; changes only when the data does, not on every clock tick.
    pub snapshot: Memo<Option<Snapshot<T>>>,
    /// `(refreshing, refresh_error)` of the loaded state.
    pub flags: Memo<(bool, bool)>,
    /// Changes when the state moves between loading, error, and loaded, and not within one of them.
    pub shape: Memo<std::mem::Discriminant<RequestState<T>>>,
    pub online: RwSignal<bool>,
    /// `Date.now()`, advanced once a second for a freshness line. Still under a polling cadence.
    pub clock: RwSignal<f64>,
    /// Starts the current key over: the loading screen, then a request.
    pub retry: Callback<()>,
    /// Asks again for the current key while leaving the loaded rows on screen.
    pub refresh: Callback<()>,
    /// Which load the current rows came from. A page that extends them itself reads this before
    /// the request it sends and compares it after, so a reply to a superseded load is discarded.
    pub generation: StoredValue<u32>,
    /// Adopts a value the page produced itself, such as a list one "load more" extended in place.
    pub adopt: Callback<T>,
}

impl<T: Send + Sync + 'static> Clone for Request<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Send + Sync + 'static> Copy for Request<T> {}

impl<T: Clone + Send + Sync + 'static> Request<T> {
    /// The loaded value, or `None` in any unloaded state.
    pub fn value(&self) -> Option<T> {
        self.snapshot.get().map(|snapshot| snapshot.value)
    }

    /// The loaded value without subscribing to it.
    pub fn value_untracked(&self) -> Option<T> {
        self.snapshot.get_untracked().map(|snapshot| snapshot.value)
    }
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
fn polling_delay(cadence: Cadence, failures: u32) -> Option<Duration> {
    let Cadence::Polling {
        minimum,
        maximum,
        backoff_step,
    } = cadence
    else {
        return None;
    };
    let maximum = maximum.as_millis() as f64;
    let lower = (minimum.as_millis() as f64
        + f64::from(failures) * backoff_step.as_millis() as f64)
        .min(maximum);
    let delay = lower + (js_sys::Math::random() * (maximum - lower + 1.0)).floor();
    Some(Duration::from_millis(delay as u64))
}

fn visible() -> bool {
    document().visibility_state() == web_sys::VisibilityState::Visible
}

/// Keeps `fetch(key)` loaded for the lifetime of the calling component, under `policy`.
pub fn use_request<K, T>(key: Memo<K>, fetch: Fetch<K, T>, policy: Policy) -> Request<T>
where
    K: Clone + PartialEq + Send + Sync + 'static,
    T: Clone + PartialEq + Send + Sync + 'static,
{
    let revision = use_console().revision;
    let state = RwSignal::new(RequestState::<T>::Loading);
    let attempt = RwSignal::new(0_u32);
    let online = RwSignal::new(window().navigator().on_line());
    let clock = RwSignal::new(js_sys::Date::now());
    let generation = StoredValue::new(0_u32);
    let in_flight = StoredValue::new(None::<u32>);
    let failures = StoredValue::new(0_u32);
    let latest = StoredValue::new(None::<Snapshot<T>>);
    let timer = StoredValue::new(None::<TimeoutHandle>);
    let polling = policy.cadence != Cadence::OnDemand;

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
            state.set(RequestState::Loaded {
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
        let Some(delay) = polling_delay(policy.cadence, failures.get_value()) else {
            return;
        };
        timer.set_value(set_timeout_with_handle(move || run_refresh(request, wanted), delay).ok());
    };
    refresh.set_value(Some(Callback::new(move |(request, wanted): (u32, K)| {
        if !is_current(request, &wanted) || in_flight.get_value() == Some(request) {
            return;
        }
        if polling && !online.get_untracked() {
            retained(false, false);
            return;
        }
        in_flight.set_value(Some(request));
        if !retained(true, false) {
            state.set(RequestState::Loading);
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
                    state.set(RequestState::Loaded {
                        snapshot,
                        refreshing: false,
                        refresh_error: false,
                    });
                }
                Ok(None) => {
                    latest.set_value(None);
                    state.set(RequestState::Unavailable);
                    return;
                }
                Err(GraphqlError::SessionExpired) => {
                    latest.set_value(None);
                    state.set(RequestState::SessionError);
                    return;
                }
                Err(GraphqlError::Transport(_)) => {
                    failures.update_value(|count| *count += 1);
                    if !retained(false, true) {
                        state.set(RequestState::Error);
                    }
                }
            }
            schedule(request, wanted);
        });
    })));

    Effect::new(move |_| {
        let wanted = key.get();
        attempt.track();
        if policy.follow_revision {
            let _ = revision.get();
        }
        clear_timer();
        let request = generation.get_value() + 1;
        generation.set_value(request);
        if policy.retention == Retention::Fresh {
            latest.set_value(None);
        }
        failures.set_value(0);
        in_flight.set_value(None);
        run_refresh(request, wanted);
    });

    if polling {
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
            on_focus.remove();
            on_online.remove();
            on_offline.remove();
            if let Some(tick) = tick {
                tick.clear();
            }
        });
    }
    on_cleanup(clear_timer);

    Request {
        state,
        snapshot: Memo::new(move |_| match state.get() {
            RequestState::Loaded { snapshot, .. } => Some(snapshot),
            _ => None,
        }),
        flags: Memo::new(move |_| match state.get() {
            RequestState::Loaded {
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
        refresh: Callback::new(move |()| {
            if let Some(wanted) = key.try_get_untracked() {
                run_refresh(generation.get_value(), wanted);
            }
        }),
        generation,
        adopt: Callback::new(move |value: T| {
            let snapshot = Snapshot {
                value,
                refreshed_at: latest
                    .try_get_value()
                    .flatten()
                    .map_or_else(js_sys::Date::now, |snapshot| snapshot.refreshed_at),
            };
            latest.set_value(Some(snapshot.clone()));
            state.set(RequestState::Loaded {
                snapshot,
                refreshing: false,
                refresh_error: false,
            });
        }),
    }
}

/// The four unloaded screens, which differ between pages only in their wording and class prefix.
pub struct UnloadedCopy {
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
    state: &RequestState<T>,
    copy: &'static UnloadedCopy,
    online: RwSignal<bool>,
    retry: Callback<()>,
) -> Option<AnyView> {
    Some(match state {
        RequestState::Loaded { .. } => return None,
        RequestState::Loading => view! {
            <main class=copy.main_class aria-label=copy.loading_label>
                {move || if online.get() {
                    view! { <section class=format!("{}-skeleton", copy.css) aria-label=copy.skeleton_label><div></div><div></div><div></div><div></div><div></div><div></div></section> }.into_any()
                } else {
                    view! { <p role="status">{copy.offline}</p> }.into_any()
                }}
            </main>
        }.into_any(),
        RequestState::SessionError => view! { <main class=copy.main_class><p role="alert">{copy.session}</p></main> }.into_any(),
        RequestState::Unavailable => view! { <main class=copy.main_class><p role="status">{copy.unavailable}</p></main> }.into_any(),
        RequestState::Error => view! {
            <main class=copy.main_class><section class=format!("{}-error", copy.css) aria-label=copy.error_label>
                <p role="alert">{copy.error}</p><button type="button" on:click=move |_| retry.run(())>"Retry"</button></section></main>
        }.into_any(),
    })
}
