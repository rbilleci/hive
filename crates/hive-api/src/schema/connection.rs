use async_graphql::SimpleObject;

/// Shared Relay-style page info. Ports the `PageInfo` type Java's `organization`/`project`
/// connections reuse (the only two domains whose connections use this exact four-field shape and
/// type name in the committed schema — `deployment`'s connections use the two-field
/// `DeploymentPageInfo`, `audit`'s use the two-field `AuditPageInfo`, and `evaluation`'s inline
/// `hasNextPage`/`endCursor` as flat fields on the connection type with no nested page-info object
/// at all; none of those three is this type, by schema, and must not be merged into it).
/// `hasPreviousPage`/`startCursor` are computed correctly here even though every
/// current Java resolver that returns a `Map`-backed `PageInfo` only ever
/// populates `hasNextPage`/`endCursor` (the console's own query documents never
/// select the other two fields, so the gap is unobservable in the Java system);
/// giving them real values rather than reproducing that gap is not a contract
/// change because no console document or E2E spec depends on the omission.
#[derive(SimpleObject)]
pub struct PageInfo {
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
}
