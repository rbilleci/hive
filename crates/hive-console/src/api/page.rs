//! One page of a Seaography connection, in the one shape every operation in `api` reports.
//!
//! Pages are numbered from 0, which is how the generated `pagination: { page }` argument counts
//! them; a URL that shows a page number to a reader adds one.

use crate::graphql::schema;

/// Seaography's page bookkeeping, returned by every connection read with `pagination: { page }`.
#[derive(cynic::QueryFragment, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaginationInfo {
    pub pages: i32,
    pub current: i32,
    pub total: i32,
}

/// Seaography's cursor marker, which reports only whether another page exists.
#[derive(cynic::QueryFragment, Debug, Clone, Copy, PartialEq, Eq)]
#[cynic(graphql_type = "PageInfo")]
pub struct GeneratedPageInfo {
    pub has_next_page: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    /// Which page `rows` is, counted from 0.
    pub page: i32,
    pub pages: i32,
    /// The rows the connection holds across every page, not the rows in this one. `None` when the
    /// connection reported only `pageInfo`, which carries no count.
    pub total: Option<i32>,
}

impl<T> Page<T> {
    /// A connection that reports no `paginationInfo` has no page after this one.
    pub fn new(rows: Vec<T>, info: Option<PaginationInfo>) -> Self {
        let info = info.unwrap_or_default();
        Self {
            rows,
            page: info.current,
            pages: info.pages,
            total: Some(info.total),
        }
    }

    /// A connection read for `pageInfo` instead of `paginationInfo`, whose page number the caller
    /// asked for rather than read back.
    pub fn from_page_info(rows: Vec<T>, page: i32, info: GeneratedPageInfo) -> Self {
        Self {
            rows,
            page,
            pages: page + 1 + i32::from(info.has_next_page),
            total: None,
        }
    }

    /// The rows the connection holds across every page, counting a connection that reports no
    /// count as the rows retrieved so far.
    pub fn total(&self) -> i32 {
        self.total
            .unwrap_or_else(|| i32::try_from(self.rows.len()).unwrap_or(i32::MAX))
    }

    pub fn has_next_page(&self) -> bool {
        self.page + 1 < self.pages
    }

    pub fn has_previous_page(&self) -> bool {
        self.page > 0
    }

    /// The page a "load more" or "next" control asks for, when there is one.
    pub fn next_page(&self) -> Option<i32> {
        self.has_next_page().then(|| self.page + 1)
    }

    /// Appends the next page's rows and adopts its position, for a list that grows in place.
    pub fn extend(&mut self, next: Self) {
        self.rows.extend(next.rows);
        self.page = next.page;
        self.pages = next.pages;
        self.total = next.total;
    }

    pub fn map<U>(self, convert: impl FnMut(T) -> U) -> Page<U> {
        Page {
            rows: self.rows.into_iter().map(convert).collect(),
            page: self.page,
            pages: self.pages,
            total: self.total,
        }
    }
}
