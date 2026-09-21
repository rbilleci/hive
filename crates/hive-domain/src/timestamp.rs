use chrono::{DateTime, Datelike, Timelike, Utc};

/// The timestamp form the `/health` responses carry: `Z` for the zero UTC offset, never
/// `+00:00`; seconds omitted on a whole minute; and a fraction of 3, 6 or 9 digits, whichever is
/// the shortest that loses nothing. The tests below pin every one of those cases.
pub fn health_timestamp_string(value: DateTime<Utc>) -> String {
    let mut out = String::with_capacity(30);

    out.push_str(&format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        value.year(),
        value.month(),
        value.day(),
        value.hour(),
        value.minute()
    ));

    let second = value.second();
    let nanos = value.nanosecond();

    if second > 0 || nanos > 0 {
        out.push_str(&format!(":{second:02}"));
        if nanos > 0 {
            out.push('.');
            if nanos.is_multiple_of(1_000_000) {
                out.push_str(&format!("{:03}", nanos / 1_000_000));
            } else if nanos.is_multiple_of(1_000) {
                out.push_str(&format!("{:06}", nanos / 1_000));
            } else {
                out.push_str(&format!("{nanos:09}"));
            }
        }
    }

    out.push('Z');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, nano: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
            .single()
            .unwrap()
            .with_nanosecond(nano)
            .unwrap()
    }

    #[test]
    fn whole_minute_has_no_seconds_or_fraction() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 0, 0)),
            "2026-09-17T08:05Z"
        );
    }

    #[test]
    fn nonzero_seconds_with_no_fraction() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 30, 0)),
            "2026-09-17T08:05:30Z"
        );
    }

    #[test]
    fn millisecond_precision_prints_three_digits() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 30, 123_000_000)),
            "2026-09-17T08:05:30.123Z"
        );
    }

    #[test]
    fn microsecond_precision_prints_six_digits() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 30, 123_456_000)),
            "2026-09-17T08:05:30.123456Z"
        );
    }

    #[test]
    fn nanosecond_precision_prints_nine_digits() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 30, 123_456_789)),
            "2026-09-17T08:05:30.123456789Z"
        );
    }

    #[test]
    fn zero_second_with_nanos_still_prints_seconds() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 0, 5_000_000)),
            "2026-09-17T08:05:00.005Z"
        );
    }

    #[test]
    fn single_digit_fields_are_zero_padded() {
        assert_eq!(
            health_timestamp_string(at(2026, 1, 2, 3, 4, 5, 0)),
            "2026-01-02T03:04:05Z"
        );
    }
}
