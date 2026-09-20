//! What a command records about the request that caused it. Audit history itself is read through
//! the generated `audit_event_projection` entity (`hive_persistence::audit`), so nothing else of
//! the audit read model lives here.

pub mod models;

pub use models::AuditRequestMetadata;
