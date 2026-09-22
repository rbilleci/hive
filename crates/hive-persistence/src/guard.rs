//! The optimistic-concurrency guard every write domain ends its update in.
//!
//! A command reads the row it is about to change `FOR UPDATE`, compares the revision the caller
//! submitted, and then writes with that same revision in the `WHERE` clause. The second check is
//! not redundant: on Aurora DSQL the lock does not block a concurrent writer, so the `WHERE`
//! clause is what makes the write lose rather than overwrite. A lost write matches no row, and
//! the command answers a revision conflict instead of a success.
//!
//! `bump` owns exactly that guard — the revision increment, the revision filter and the
//! row-count check. The payload columns, the key filter and the refusal stay with the command,
//! which is the part that differs between them.

use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, UpdateMany};

/// Applies `update` to the row it already selects, advancing `revision` by one, but only while
/// that row still holds `expected`. `false` when it no longer does: a concurrent writer moved the
/// row on and this command lost.
///
/// `update` arrives carrying its payload columns and its key filter, so the caller reads as the
/// write it is. `revision` names the column because the domains do not agree on one: it is
/// `revision` in most tables, `current_revision` in the two policy tables, and
/// `current_draft_revision` on `reusable_resources`.
pub async fn bump<E>(
    db: &impl ConnectionTrait,
    update: UpdateMany<E>,
    revision: E::Column,
    expected: i64,
) -> Result<bool, DbErr>
where
    E: EntityTrait,
{
    let updated = update
        .col_expr(revision, Expr::col(revision).add(1))
        .filter(revision.eq(expected))
        .exec(db)
        .await?;
    Ok(updated.rows_affected == 1)
}
