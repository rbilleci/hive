//! The text helpers the pages share: browser-local time, ISO time, URL-component escaping, list
//! joining, and the role an inline message carries.

/// The browser's locale rendering of an ISO timestamp.
pub fn local_time(value: &str) -> String {
    String::from(
        js_sys::Date::new(&value.into())
            .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED),
    )
}

/// `local_time` for a timestamp the server may not have recorded.
pub fn display_time(value: Option<&str>) -> String {
    value.map_or_else(|| "Not recorded".to_string(), local_time)
}

/// The browser's current time, in its own locale rendering.
pub fn local_now() -> String {
    String::from(
        js_sys::Date::new_0().to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED),
    )
}

/// An ISO timestamp normalised through the browser's parser, so every page spells one instant the
/// same way whatever form the server sent.
pub fn iso(value: &str) -> String {
    String::from(js_sys::Date::new(&value.into()).to_iso_string())
}

/// `iso` for a `Date.now()` reading.
pub fn iso_millis(value: f64) -> String {
    String::from(js_sys::Date::new(&value.into()).to_iso_string())
}

/// Escapes a value for one query-string parameter or one path segment.
pub fn encode(value: &str) -> String {
    String::from(js_sys::encode_uri_component(value))
}

/// The values joined by `", "`, or `empty` when there are none.
pub fn joined_or<T: ToString>(values: &[T], empty: &str) -> String {
    if values.is_empty() {
        empty.to_string()
    } else {
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A fresh identifier for one command, so a resubmission after an unknown outcome reuses it.
pub fn random_uuid() -> String {
    leptos::prelude::window()
        .crypto()
        .map(|crypto| crypto.random_uuid())
        .unwrap_or_default()
}

/// A message that reports a failure is an alert; anything else is a status.
pub fn message_role(message: &str) -> &'static str {
    if message.contains("could") {
        "alert"
    } else {
        "status"
    }
}
