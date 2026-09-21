//! Optional free text as every command treats it: surrounding whitespace is not content, and
//! whitespace alone is not a value. `hive-persistence` reaches this module too, so the trimming
//! rule a command validates against and the one a write stores are the same rule.

/// `value` trimmed, or `None` when it is absent or holds nothing but whitespace.
pub fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
