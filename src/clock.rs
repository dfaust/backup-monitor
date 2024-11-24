use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use chrono::prelude::*;
use fake::{faker::chrono::en::DateTimeBetween, Dummy, Fake, Faker};
use rand::Rng;

static FAKE_CLOCKS: LazyLock<Mutex<HashMap<u64, FakeClock>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

enum FakeClock {
    Static(DateTime<Utc>),
}

#[derive(Debug, Clone, Copy)]
pub struct Clock(Option<u64>);

impl Clock {
    pub fn new() -> Clock {
        Clock(None)
    }

    pub fn with_time<Tz: TimeZone>(time: DateTime<Tz>) -> Clock {
        let id = rand::random::<u64>();
        FAKE_CLOCKS
            .lock()
            .unwrap()
            .insert(id, FakeClock::Static(time.with_timezone(&Utc)));
        Clock(Some(id))
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.0.map_or_else(Utc::now, |id| {
            match FAKE_CLOCKS.lock().unwrap().get_mut(&id).unwrap() {
                FakeClock::Static(time) => *time,
            }
        })
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl Dummy<Faker> for Clock {
    fn dummy_with_rng<R: Rng + ?Sized>(_: &Faker, rng: &mut R) -> Self {
        let now = DateTimeBetween(
            Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2200, 1, 1, 0, 0, 0).unwrap(),
        )
        .fake_with_rng(rng);
        Clock::with_time(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn real_clock() {
        let clock = Clock::new();

        assert!(clock.now() < Utc::now() + Duration::milliseconds(1));
        assert!(clock.now() + Duration::milliseconds(1) > Utc::now());
    }

    #[test]
    fn static_clock() {
        let time = Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap();
        let clock = Clock::with_time(time);

        assert_eq!(clock.now(), time);
        assert_eq!(clock.now(), time);
        assert_eq!(clock.now(), time);
    }

    #[test]
    fn fake_clock() {
        let clock = Faker.fake::<Clock>();

        assert_eq!(clock.now(), clock.now());
    }
}
