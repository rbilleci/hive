//! The one error every repository boundary in this crate answers with when it has neither a
//! result nor a typed domain refusal to give.
//!
//! The persistence layer classifies a store failure into one of these variants at its own edge;
//! transports branch on the variant rather than treating every failure alike.

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    /// The store could not answer: a failed statement, a lost connection, or a local engine that
    /// could not produce a decision. Nothing the caller changes about the request helps.
    #[error("{0}")]
    Unavailable(String),
    /// A serialization failure or a unique violation the command did not expect. Repeating the
    /// request can succeed, so reporting it as a dependency failure would send the caller into a
    /// retry loop against a request that will never differ.
    #[error("{0}")]
    Conflict(String),
    /// A row the command named is not there.
    #[error("{0}")]
    NotFound(String),
}
