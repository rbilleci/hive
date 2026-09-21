//! The one transport every console request uses: `POST /graphql` with the session cookie.

use cynic::{GraphQlResponse, Operation};
use serde::de::DeserializeOwned;
use serde::Serialize;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

#[cynic::schema("hive")]
pub mod schema {}

cynic::impl_scalar!(serde_json::Value, schema::JSON);

/// Seaography's `Json` scalar. A newtype because one Rust type can stand for only one GraphQL
/// scalar, and `serde_json::Value` already stands for the hand-built `JSON`; this goes away with
/// that scalar.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(transparent)]
pub struct GeneratedJson(pub serde_json::Value);
cynic::impl_scalar!(GeneratedJson, schema::Json);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphqlError {
    /// HTTP 401: the session cookie is missing, expired, or not signed by this service.
    SessionExpired,
    /// Any other failure: a non-2xx status, a GraphQL `errors` entry, a missing `data`, or no response.
    Transport(String),
}

/// A failed request with its HTTP status, for a page that words 403 and 503 differently.
/// `status` is 0 when no response arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportFailure {
    pub status: u16,
    pub message: String,
}

impl From<TransportFailure> for GraphqlError {
    fn from(failure: TransportFailure) -> Self {
        if failure.status == 401 {
            Self::SessionExpired
        } else {
            Self::Transport(failure.message)
        }
    }
}

pub async fn execute<Data, Variables>(
    operation: Operation<Data, Variables>,
) -> Result<Data, GraphqlError>
where
    Data: DeserializeOwned + 'static,
    Variables: Serialize,
{
    Ok(send(operation, None).await?)
}

/// `execute`, abandoned with a transport error when no response arrives within `millis`.
pub async fn execute_within<Data, Variables>(
    operation: Operation<Data, Variables>,
    millis: i32,
) -> Result<Data, GraphqlError>
where
    Data: DeserializeOwned + 'static,
    Variables: Serialize,
{
    Ok(send(operation, Some(millis)).await?)
}

pub async fn execute_with_status<Data, Variables>(
    operation: Operation<Data, Variables>,
) -> Result<Data, TransportFailure>
where
    Data: DeserializeOwned + 'static,
    Variables: Serialize,
{
    send(operation, None).await
}

/// Clears the deadline timer however the request ends.
struct Deadline(Option<i32>);

impl Drop for Deadline {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            leptos::prelude::window().clear_timeout_with_handle(handle);
        }
    }
}

// `fetch` receives the URL as a string, so a page script that
// intercepts `/graphql` by URL sees these requests too.
async fn send<Data, Variables>(
    operation: Operation<Data, Variables>,
    deadline: Option<i32>,
) -> Result<Data, TransportFailure>
where
    Data: DeserializeOwned + 'static,
    Variables: Serialize,
{
    let failure = |status: u16, message: &str| TransportFailure {
        status,
        message: message.to_string(),
    };
    let unsent = || failure(0, "The GraphQL request could not be sent.");
    let body = serde_json::to_string(&operation).map_err(|_| unsent())?;
    let headers = web_sys::Headers::new().map_err(|_| unsent())?;
    headers
        .set("content-type", "application/json")
        .map_err(|_| unsent())?;
    let controller = web_sys::AbortController::new().map_err(|_| unsent())?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_credentials(web_sys::RequestCredentials::Include);
    init.set_headers(&headers);
    init.set_body(&body.into());
    init.set_signal(Some(&controller.signal()));
    let window = leptos::prelude::window();
    let _timer = Deadline(deadline.and_then(|millis| {
        let abort = Closure::once_into_js(move || controller.abort());
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(abort.unchecked_ref(), millis)
            .ok()
    }));
    let response = JsFuture::from(window.fetch_with_str_and_init("/graphql", &init))
        .await
        .map_err(|error| {
            let aborted = error
                .dyn_ref::<web_sys::DomException>()
                .is_some_and(|error| error.name() == "AbortError");
            failure(
                0,
                if aborted {
                    "The request timed out."
                } else {
                    "The GraphQL service could not be reached."
                },
            )
        })?;
    let response: web_sys::Response = response.unchecked_into();
    let status = response.status();
    if status == 401 {
        return Err(failure(status, "Your session has expired."));
    }
    let invalid = || failure(status, "The GraphQL service returned an invalid response.");
    let text = JsFuture::from(response.text().map_err(|_| invalid())?)
        .await
        .ok()
        .and_then(|text| text.as_string())
        .ok_or_else(invalid)?;
    let envelope: GraphQlResponse<Data> = serde_json::from_str(&text).map_err(|_| invalid())?;
    if let Some(error) = envelope.errors.as_ref().and_then(|errors| errors.first()) {
        return Err(failure(status, &error.message));
    }
    match envelope.data {
        Some(data) if response.ok() => Ok(data),
        _ => Err(failure(
            status,
            &format!("The GraphQL request failed with HTTP {status}."),
        )),
    }
}
