//! The domain tier: the rules, the services that apply them, and the ports the persistence tier
//! implements. Nothing here names a database or a transport — no `sea-orm`, no `async-graphql`,
//! no `axum` — so a service is exercised against a stub port with no PostgreSQL in reach.
//! `scripts/architecture.mjs` checks that claim by scanning the manifests.
//!
//! Each domain is laid out the same way, so a file name means the same thing everywhere:
//!
//! * `mod.rs` — the domain's public surface: its modules, and the `pub use` list a caller in
//!   `hive-api` or `hive` names instead of reaching into a submodule.
//! * `models.rs` — the domain's own types: its command inputs, its results, and its typed
//!   refusals. Never a stored row; a port that answers with one is generic over it.
//! * `repository.rs` — the persistence port, as a trait the service is generic over. This is the
//!   dependency inversion the crate exists for: `hive-persistence` depends on this crate to
//!   implement these traits, not the other way round.
//! * `service.rs` — the application service: it validates a command's shape, applies the rules,
//!   and calls the port. Generic over the port, so it constructs in a unit test.
//!
//! A domain with rules or an engine worth naming keeps it beside those, in its own file or its
//! own directory: `administration::rules`, `deployment::{compiler, policy, status, worker}`,
//! `evaluation::{document, scoring, state_machine, outcome, decider, fixture, worker}`,
//! `agent::canonical_document`, `configuration::{canonical, identity}`.
//!
//! Three modules are deliberately not that shape. `audit` carries only `models`: what a command
//! records about the request that caused it, with no command of its own. `text` is the one
//! trimming rule every command and every write shares. `repository` is crate-wide rather than a
//! domain: the single `RepositoryError` every port answers with.
//!
//! One port sits outside a `repository.rs` on purpose, because it is not a persistence boundary:
//! `evaluation::fixture::EvaluationFixturePort` is the prompt-case runner, implemented in this
//! crate by `LocalPromptCaseFixtureAdapter`. `deployment::repository` holds a second trait
//! alongside its domain port, `DeploymentOutboxDelivery` — the outbox worker's single unit of
//! work, kept there because it is still a boundary `hive-persistence` implements.

pub mod administration;
pub mod agent;
pub mod audit;
pub mod configuration;
pub mod console;
pub mod deployment;
pub mod evaluation;
pub mod repository;
pub mod text;

pub use repository::RepositoryError;
