//! Reading the times events carry.

/// Milliseconds since the Unix epoch for an RFC 3339 time in UTC, such as
/// `2026-11-17T09:15:11.204Z`, or `None` when the text is not one.
pub fn millis(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, clock) = text.split_once('T')?;
    let mut date = date.splitn(3, '-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut clock = clock.splitn(3, ':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..=60).contains(&second)
        || !fraction.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    // The first three digits of the fraction, padded: `.2` is 200 ms.
    let fraction: String = fraction.chars().chain("000".chars()).take(3).collect();
    let fraction: i64 = fraction.parse().ok()?;
    // Days since 1970-01-01, by the civil-calendar arithmetic that counts
    // years from March so the leap day falls at a year's end.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + fraction)
}

/// How long it was from `start` to `end`, in milliseconds, when both are
/// times and the second is not before the first.
pub fn between(start: &str, end: &str) -> Option<u64> {
    u64::try_from(millis(end)? - millis(start)?).ok()
}

/// A duration for a person: `840 ms`, `12.4 s`, `3 m 05 s`, `1 h 02 m`.
pub fn human(ms: u64) -> String {
    let seconds = ms / 1000;
    if ms < 1000 {
        format!("{ms} ms")
    } else if seconds < 60 {
        format!("{}.{} s", seconds, (ms % 1000) / 100)
    } else if seconds < 3600 {
        format!("{} m {:02} s", seconds / 60, seconds % 60)
    } else {
        format!("{} h {:02} m", seconds / 3600, (seconds % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_is_read_as_milliseconds_since_the_epoch() {
        assert_eq!(millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(millis("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(millis("2000-03-01T00:00:00.000Z"), Some(951_868_800_000));
        assert_eq!(millis("2026-11-17T09:15:11.204Z"), Some(1_794_906_911_204));
        for bad in [
            "",
            "yesterday",
            "2026-11-17",
            "2026-13-01T00:00:00Z",
            "2026-11-17T09:15:11",
        ] {
            assert_eq!(millis(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_duration_is_the_gap_between_two_times_and_reads_naturally() {
        assert_eq!(
            between("2026-11-17T09:15:11.204Z", "2026-11-17T09:15:12.004Z"),
            Some(800)
        );
        assert_eq!(
            between("2026-11-17T09:15:12Z", "2026-11-17T09:15:11Z"),
            None
        );
        assert_eq!(between("", "2026-11-17T09:15:11Z"), None);
        assert_eq!(human(840), "840 ms");
        assert_eq!(human(12_430), "12.4 s");
        assert_eq!(human(185_000), "3 m 05 s");
        assert_eq!(human(3_720_000), "1 h 02 m");
    }
}
