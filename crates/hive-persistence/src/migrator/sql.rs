use regex::Regex;
use std::sync::LazyLock;

static CREATE_INDEX_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(\s*CREATE\s+(?:UNIQUE\s+)?INDEX)(\s+)").unwrap());
static INDEX_SORT_ORDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(ASC|DESC)\b").unwrap());
static ADD_CHECK_CONSTRAINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)ALTER\s+TABLE\s+(?:IF\s+EXISTS\s+)?(\S+)\s+ADD\s+CONSTRAINT\s+(\S+)\s+CHECK\s*\(",
    )
    .unwrap()
});

/// Splits a migration file's raw text into individual statements on top-level `;`
/// boundaries, honoring `'...'` string literals (with `''` as an escaped quote) and
/// `--` line comments. The files under `db/migration` use no other comment style and no
/// dollar-quoting, so nothing here handles either.
pub fn split_statements(source: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut chars = source.chars().peekable();

    while let Some(c) = chars.next() {
        if in_string {
            current.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    current.push(chars.next().unwrap());
                } else {
                    in_string = false;
                }
            }
            continue;
        }

        match c {
            '\'' => {
                in_string = true;
                current.push(c);
            }
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for next in chars.by_ref() {
                    if next == '\n' {
                        current.push('\n');
                        break;
                    }
                }
            }
            ';' => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    statements.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }

    statements
}

/// True when a statement's first non-blank, non-comment line starts with
/// `CREATE [UNIQUE] INDEX`.
pub fn is_create_index_statement(sql: &str) -> bool {
    first_content_line(sql)
        .map(|line| CREATE_INDEX_LINE.is_match(line))
        .unwrap_or(false)
}

fn first_content_line(sql: &str) -> Option<&str> {
    sql.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
}

/// Inserts `ASYNC` after `CREATE [UNIQUE] INDEX` on the statement's first content line. Used
/// only against real Aurora DSQL; plain PostgreSQL executes the statement unmodified.
pub fn inject_async_keyword(sql: &str) -> String {
    let mut replaced_once = false;
    sql.lines()
        .map(|line| {
            let trimmed = line.trim();
            if replaced_once || trimmed.is_empty() || trimmed.starts_with("--") {
                line.to_string()
            } else {
                replaced_once = true;
                CREATE_INDEX_LINE.replace(line, "$1 ASYNC$2").into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Removes `ASC`/`DESC` index-column sort-order keywords. Aurora DSQL's asynchronous index build
/// rejects explicit sort order; PostgreSQL keeps them.
pub fn strip_index_sort_order(sql: &str) -> String {
    INDEX_SORT_ORDER.replace_all(sql, "").into_owned()
}

pub struct CheckConstraint<'a> {
    pub table: &'a str,
    pub name: &'a str,
}

/// Matches `ALTER TABLE [IF EXISTS] <table> ADD CONSTRAINT <name> CHECK (`. Every
/// `ADD CONSTRAINT` statement under `db/migration` names its table and constraint with a plain
/// unquoted identifier, so the capture handles no quoted form.
pub fn match_add_check_constraint(sql: &str) -> Option<CheckConstraint<'_>> {
    ADD_CHECK_CONSTRAINT
        .captures(sql)
        .map(|captures| CheckConstraint {
            table: captures.get(1).unwrap().as_str(),
            name: captures.get(2).unwrap().as_str(),
        })
}

/// Appends `NOT VALID` unless the statement already ends with it.
pub fn ensure_not_valid(sql: &str) -> String {
    let trimmed = sql.trim_end();
    if trimmed.to_uppercase().ends_with("NOT VALID") {
        trimmed.to_string()
    } else {
        format!("{trimmed} NOT VALID")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_top_level_semicolons() {
        let statements =
            split_statements("CREATE TABLE t (id UUID); INSERT INTO t VALUES ('a;b');");
        assert_eq!(
            statements,
            vec!["CREATE TABLE t (id UUID)", "INSERT INTO t VALUES ('a;b')"]
        );
    }

    #[test]
    fn ignores_semicolons_inside_line_comments() {
        let statements = split_statements("-- a; b\nCREATE TABLE t (id UUID);");
        assert_eq!(statements, vec!["CREATE TABLE t (id UUID)"]);
    }

    #[test]
    fn handles_escaped_quotes_inside_string_literals() {
        let statements = split_statements("INSERT INTO t VALUES ('it''s; here');");
        assert_eq!(statements, vec!["INSERT INTO t VALUES ('it''s; here')"]);
    }

    #[test]
    fn detects_create_index_case_insensitively() {
        assert!(is_create_index_statement(
            "-- comment\nCREATE UNIQUE INDEX foo ON t (a)"
        ));
        assert!(is_create_index_statement("create index foo on t (a)"));
        assert!(!is_create_index_statement(
            "ALTER TABLE t ADD COLUMN a TEXT"
        ));
    }

    #[test]
    fn injects_async_after_index_keyword() {
        assert_eq!(
            inject_async_keyword("CREATE INDEX foo ON t (a)"),
            "CREATE INDEX ASYNC foo ON t (a)"
        );
        assert_eq!(
            inject_async_keyword("CREATE UNIQUE INDEX foo ON t (a)"),
            "CREATE UNIQUE INDEX ASYNC foo ON t (a)"
        );
    }

    #[test]
    fn strips_sort_order_keywords() {
        assert_eq!(
            strip_index_sort_order("CREATE INDEX foo ON t (a ASC, b DESC)"),
            "CREATE INDEX foo ON t (a, b)"
        );
    }

    #[test]
    fn matches_add_check_constraint() {
        let matched = match_add_check_constraint(
            "ALTER TABLE deployments ADD CONSTRAINT deployments_nn CHECK (x IS NOT NULL)",
        )
        .unwrap();
        assert_eq!(matched.table, "deployments");
        assert_eq!(matched.name, "deployments_nn");
    }

    #[test]
    fn ensure_not_valid_is_idempotent() {
        assert_eq!(ensure_not_valid("CHECK (x > 0)"), "CHECK (x > 0) NOT VALID");
        assert_eq!(
            ensure_not_valid("CHECK (x > 0) NOT VALID"),
            "CHECK (x > 0) NOT VALID"
        );
    }
}
