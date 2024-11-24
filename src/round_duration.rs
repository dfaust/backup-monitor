use chrono::Duration;

pub enum RoundAccuracy {
    Minutes,
    Seconds,
}

pub enum RoundDirection {
    Up,
    Down,
}

pub fn round_duration(
    duration: Duration,
    accuracy: RoundAccuracy,
    direction: RoundDirection,
) -> (Duration, Duration) {
    if duration >= Duration::days(1) {
        // round to next hour
        let hours = duration.num_hours();
        let rest = duration.num_milliseconds() - hours * 60 * 60 * 1_000;
        if rest == 0 {
            return (duration, Duration::zero());
        }
        match direction {
            RoundDirection::Up => (
                Duration::hours(hours + 1),
                Duration::milliseconds(60 * 60 * 1_000 - rest),
            ),
            RoundDirection::Down => (Duration::hours(hours), Duration::milliseconds(rest)),
        }
    } else if duration >= Duration::hours(1) || matches!(accuracy, RoundAccuracy::Minutes) {
        // round to next minute
        let minutes = duration.num_minutes();
        let rest = duration.num_milliseconds() - minutes * 60 * 1_000;
        if rest == 0 {
            return (duration, Duration::zero());
        }
        match direction {
            RoundDirection::Up => (
                Duration::minutes(minutes + 1),
                Duration::milliseconds(60 * 1_000 - rest),
            ),
            RoundDirection::Down => (Duration::minutes(minutes), Duration::milliseconds(rest)),
        }
    } else {
        // round to next second
        let seconds = duration.num_seconds();
        let rest = duration.num_milliseconds() - seconds * 1_000;
        if rest == 0 {
            return (duration, Duration::zero());
        }
        match direction {
            RoundDirection::Up => (
                Duration::seconds(seconds + 1),
                Duration::milliseconds(1_000 - rest),
            ),
            RoundDirection::Down => (Duration::seconds(seconds), Duration::milliseconds(rest)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_to_minutes() {
        fn duration(s: &str) -> Duration {
            Duration::from_std(humantime::parse_duration(s).unwrap()).unwrap()
        }

        assert_eq!(
            round_duration(duration("1d"), RoundAccuracy::Minutes, RoundDirection::Down),
            (duration("1d"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1h"), RoundAccuracy::Minutes, RoundDirection::Down),
            (duration("1h"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1m"), RoundAccuracy::Minutes, RoundDirection::Down),
            (duration("1m"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1s"), RoundAccuracy::Minutes, RoundDirection::Down),
            (Duration::zero(), duration("1s"))
        );

        assert_eq!(
            round_duration(
                duration("1d 17h"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("1d 17h"), Duration::zero())
        );
        assert_eq!(
            round_duration(
                duration("2h 59m"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("2h 59m"), Duration::zero())
        );

        assert_eq!(
            round_duration(
                duration("1d 17h 9m"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("1d 17h"), duration("9m"))
        );
        assert_eq!(
            round_duration(
                duration("1d 9m"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("1d"), duration("9m"))
        );
        assert_eq!(
            round_duration(
                duration("1d 9m 389ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("1d"), duration("9m 389ms"))
        );

        assert_eq!(
            round_duration(
                duration("5h 9m 17s"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("5h 9m"), duration("17s"))
        );
        assert_eq!(
            round_duration(
                duration("5h 17s"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("5h"), duration("17s"))
        );
        assert_eq!(
            round_duration(
                duration("5h 17s 389ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("5h"), duration("17s 389ms"))
        );

        assert_eq!(
            round_duration(
                duration("29m 8s 28ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("29m"), duration("8s 28ms"))
        );
        assert_eq!(
            round_duration(
                duration("15m 16s"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("15m"), duration("16s"))
        );
        assert_eq!(
            round_duration(
                duration("29m 28ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("29m"), duration("28ms"))
        );

        assert_eq!(
            round_duration(
                duration("34s 127ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (Duration::zero(), duration("34s 127ms"))
        );
        assert_eq!(
            round_duration(
                duration("34s 94ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (Duration::zero(), duration("34s 94ms"))
        );
    }

    #[test]
    fn round_to_seconds() {
        fn duration(s: &str) -> Duration {
            Duration::from_std(humantime::parse_duration(s).unwrap()).unwrap()
        }

        assert_eq!(
            round_duration(duration("1d"), RoundAccuracy::Seconds, RoundDirection::Down),
            (duration("1d"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1h"), RoundAccuracy::Seconds, RoundDirection::Down),
            (duration("1h"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1m"), RoundAccuracy::Seconds, RoundDirection::Down),
            (duration("1m"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1s"), RoundAccuracy::Seconds, RoundDirection::Down),
            (duration("1s"), Duration::zero())
        );

        assert_eq!(
            round_duration(
                duration("1d 17h"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("1d 17h"), Duration::zero())
        );
        assert_eq!(
            round_duration(
                duration("2h 59m"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("2h 59m"), Duration::zero())
        );

        assert_eq!(
            round_duration(
                duration("1d 17h 9m"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("1d 17h"), duration("9m"))
        );
        assert_eq!(
            round_duration(
                duration("1d 9m"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("1d"), duration("9m"))
        );
        assert_eq!(
            round_duration(
                duration("1d 9m 389ms"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("1d"), duration("9m 389ms"))
        );

        assert_eq!(
            round_duration(
                duration("5h 9m 17s"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("5h 9m"), duration("17s"))
        );
        assert_eq!(
            round_duration(
                duration("5h 17s"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("5h"), duration("17s"))
        );
        assert_eq!(
            round_duration(
                duration("5h 17s 389ms"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("5h"), duration("17s 389ms"))
        );

        assert_eq!(
            round_duration(
                duration("29m 8s 28ms"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("29m 8s"), duration("28ms"))
        );
        assert_eq!(
            round_duration(
                duration("15m 16s"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("15m 16s"), Duration::zero())
        );
        assert_eq!(
            round_duration(
                duration("29m 28ms"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("29m"), duration("28ms"))
        );

        assert_eq!(
            round_duration(
                duration("34s 127ms"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("34s"), duration("127ms"))
        );
        assert_eq!(
            round_duration(
                duration("34s 94ms"),
                RoundAccuracy::Seconds,
                RoundDirection::Down
            ),
            (duration("34s"), duration("94ms"))
        );
    }

    #[test]
    fn round_up() {
        fn duration(s: &str) -> Duration {
            Duration::from_std(humantime::parse_duration(s).unwrap()).unwrap()
        }

        assert_eq!(
            round_duration(duration("1d"), RoundAccuracy::Minutes, RoundDirection::Up),
            (duration("1d"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1h"), RoundAccuracy::Minutes, RoundDirection::Up),
            (duration("1h"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1m"), RoundAccuracy::Minutes, RoundDirection::Up),
            (duration("1m"), Duration::zero())
        );
        assert_eq!(
            round_duration(duration("1s"), RoundAccuracy::Minutes, RoundDirection::Up),
            (duration("1m"), duration("59s"))
        );

        assert_eq!(
            round_duration(
                duration("1d 17h"),
                RoundAccuracy::Minutes,
                RoundDirection::Up
            ),
            (duration("1d 17h"), Duration::zero())
        );
        assert_eq!(
            round_duration(
                duration("2h 59m"),
                RoundAccuracy::Minutes,
                RoundDirection::Up
            ),
            (duration("2h 59m"), Duration::zero())
        );
        assert_eq!(
            round_duration(
                duration("2h 59m 13ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Up
            ),
            (duration("3h"), duration("59s 987ms"))
        );
        assert_eq!(
            round_duration(
                duration("2h 13ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Up
            ),
            (duration("2h 1m"), duration("59s 987ms"))
        );

        assert_eq!(
            round_duration(
                duration("1d 17h 9m"),
                RoundAccuracy::Minutes,
                RoundDirection::Up
            ),
            (duration("1d 18h"), duration("51m"))
        );
        assert_eq!(
            round_duration(
                duration("1d 9m"),
                RoundAccuracy::Minutes,
                RoundDirection::Up
            ),
            (duration("1d 1h"), duration("51m"))
        );
        assert_eq!(
            round_duration(
                duration("1d 9m 389ms"),
                RoundAccuracy::Minutes,
                RoundDirection::Up
            ),
            (duration("1d 1h"), duration("50m 59s 611ms"))
        );
    }
}
