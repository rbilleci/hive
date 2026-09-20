//! `currentPrincipal`, unchanged from the static tier (`schema/mod.rs`'s `CoreQueries`).

use crate::schema::scalars::Id;
use crate::schema::RequestPrincipal;
use async_graphql::Context;
use seaography::CustomOutputType;

// Wire case: `CustomFields`/`CustomOutputType` spell a GraphQL name from the Rust identifier
// verbatim (`GSR-WIRE-CASE`), so this module's identifiers are written the way they must appear
// on the wire.
#[allow(non_snake_case)]
mod wire {
    use super::*;

    #[derive(CustomOutputType, Clone)]
    pub struct Principal {
        pub id: Id,
        pub subject: String,
    }

    pub struct CoreQueries;

    #[seaography::CustomFields]
    impl CoreQueries {
        async fn currentPrincipal(ctx: &Context<'_>) -> async_graphql::Result<Principal> {
            let principal = ctx.data::<RequestPrincipal>().map_err(|_| {
                async_graphql::Error::new("An authenticated principal is required.")
            })?;
            let text = principal.0.to_string();
            Ok(Principal {
                id: text.clone().into(),
                subject: text,
            })
        }
    }
}

pub use wire::{CoreQueries, Principal};
