use std::{
    collections::HashMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Instant,
};

use arc_swap::ArcSwap;
use chrono::{DateTime, Duration, Utc};
use notify_rust::{Hint, Notification, Timeout};

use crate::{
    settings::{Script, Settings},
    tray::Tray,
    RETRY_INTERVAL,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptState {
    WaitingForTime,
    WaitingForPath(PathBuf),
    Running,
    Failed(DateTime<Utc>, String),
}

pub struct Manager {
    settings: Arc<ArcSwap<Settings>>,
    states: HashMap<String, ScriptState>,
}

impl Manager {
    pub fn new(settings: Arc<ArcSwap<Settings>>) -> Manager {
        Manager {
            settings,
            states: HashMap::new(),
        }
    }

    fn script_state(&self, script: &Script) -> ScriptState {
        match self.states.get(&script.name) {
            Some(ScriptState::WaitingForPath(path)) if *path != script.backup_path => {
                ScriptState::WaitingForTime
            }
            Some(state) => state.clone(),
            None => ScriptState::WaitingForTime,
        }
    }

    pub fn next_backup(&self) -> Option<DateTime<Utc>> {
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter_map(|script| match self.script_state(script) {
                ScriptState::WaitingForTime => Some(next_backup(script)),
                ScriptState::Failed(ts, _) => Some(ts + RETRY_INTERVAL),
                _ => None,
            })
            .min()
    }

    pub fn next_reminder(&self) -> Option<DateTime<Utc>> {
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter(|script| self.script_state(script) != ScriptState::Running)
            .filter_map(next_reminder)
            .min()
    }

    pub fn next_ui_update(&self) -> Option<DateTime<Utc>> {
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter(|script| self.script_state(script) == ScriptState::WaitingForTime)
            .map(next_ui_update)
            .min()
    }

    pub fn tooltip(&self) -> String {
        let mut items = Vec::new();

        let settings = self.settings.load();

        if settings.scripts.is_empty() {
            items.push("No backup scripts configured".to_string());
        } else {
            for script in &settings.scripts {
                items.push(format!(
                    "{}:\n{}",
                    script.name,
                    tooltip(
                        script,
                        self.states
                            .get(&script.name)
                            .unwrap_or(&ScriptState::WaitingForTime)
                    )
                ));
            }
        }

        items.join("\n\n")
    }

    pub fn run(
        &mut self,
        script_name: Option<&str>,
        handle: &ksni::Handle<Tray>,
    ) -> anyhow::Result<()> {
        let settings = self.settings.load_full();

        for script in &settings.scripts {
            if script_name.is_some_and(|name| name == script.name)
                || (script_name.is_none() && next_backup(script) <= Utc::now())
            {
                if is_mounted(&script.backup_path)? {
                    log::info!("running backup script `{}`", script.script_path.display());

                    self.states
                        .insert(script.name.clone(), ScriptState::Running);

                    let mut notification_handle = Notification::new()
                        .appname(&settings.title)
                        .summary(&format!("Starting {}", script.name))
                        .icon(&settings.icon_name)
                        .hint(Hint::Resident(true))
                        .timeout(Timeout::Never)
                        .show()?;

                    handle.update(|tray| {
                        tray.set_status(ksni::Status::Active);
                        tray.set_tooltip(self.tooltip());
                    });

                    let start = Instant::now();

                    let state;
                    let summary;
                    let body;
                    match Command::new(&script.script_path).status() {
                        Ok(status) => {
                            if status.success() {
                                let (run_duration, _) = round_duration(
                                    Duration::from_std(start.elapsed())?,
                                    RoundAccuracy::Seconds,
                                    RoundDirection::Down,
                                );
                                summary = format!("{} finished", script.name);
                                body = format!(
                                    "Backup took {}",
                                    humantime::format_duration(run_duration.to_std()?)
                                );
                                state = ScriptState::WaitingForTime;

                                // get latest settings
                                let mut settings = Arc::unwrap_or_clone(self.settings.load_full());

                                // find script and update `last_backup`
                                if let Some(script) =
                                    settings.scripts.iter_mut().find(|s| s.name == script.name)
                                {
                                    script.last_backup = Some(Utc::now());
                                }

                                // save new settings
                                settings.save()?;
                            } else if let Some(code) = status.code() {
                                summary = format!("{} failed with exit code {code}", script.name);
                                body = String::new();
                                state = ScriptState::Failed(Utc::now(), summary.clone());
                            } else {
                                summary = format!("{} failed", script.name);
                                body = String::new();
                                state = ScriptState::Failed(Utc::now(), summary.clone());
                            }
                        }
                        Err(error) => {
                            summary = format!("{} failed with error", script.name);
                            body = error.to_string();
                            state = ScriptState::Failed(Utc::now(), error.to_string());
                        }
                    };

                    self.states.insert(script.name.clone(), state);

                    notification_handle.summary(&summary);
                    notification_handle.body(&body);
                    notification_handle.timeout(Timeout::Milliseconds(6_000));
                    notification_handle.update();
                } else {
                    log::debug!("waiting for `{}` to appear", script.backup_path.display());

                    self.states.insert(
                        script.name.clone(),
                        ScriptState::WaitingForPath(script.backup_path.clone()),
                    );
                }
            }

            handle.update(|tray| {
                tray.set_tooltip(self.tooltip());
            });
        }

        Ok(())
    }
}

enum RoundAccuracy {
    Minutes,
    Seconds,
}

enum RoundDirection {
    Up,
    Down,
}

fn round_duration(
    duration: Duration,
    accuracy: RoundAccuracy,
    direction: RoundDirection,
) -> (Duration, Duration) {
    let (add_secs, secs, nanos) = if duration >= Duration::days(1) {
        let secs = duration.num_seconds() % 3600;
        let nanos = duration.subsec_nanos();
        match direction {
            RoundDirection::Up if nanos > 0 => (3600, 3600 - secs, 1_000_000_000 - nanos),
            _ => (0, secs, nanos),
        }
    } else if duration >= Duration::hours(1) || matches!(accuracy, RoundAccuracy::Minutes) {
        let secs = duration.num_seconds() % 60;
        let nanos = duration.subsec_nanos();
        match direction {
            RoundDirection::Up if nanos > 0 => (60, 60 - secs, 1_000_000_000 - nanos),
            _ => (0, secs, nanos),
        }
    } else {
        let secs = duration.num_seconds();
        let nanos = duration.subsec_nanos();
        match direction {
            RoundDirection::Up if nanos > 0 => (0, secs, 1_000_000_000 - nanos),
            _ => (0, secs, nanos),
        }
    };

    let remainder = Duration::new(secs, nanos as u32).unwrap();

    (
        duration + Duration::seconds(add_secs) - remainder,
        remainder,
    )
}

fn next_backup(script: &Script) -> DateTime<Utc> {
    script
        .last_backup
        .as_ref()
        .map_or_else(Utc::now, |last_backup| *last_backup + script.interval)
}

fn next_ui_update(script: &Script) -> DateTime<Utc> {
    let now = Utc::now();

    let (_, next_backup_remainder) = round_duration(
        next_backup(script).max(now) - now,
        RoundAccuracy::Minutes,
        RoundDirection::Down,
    );

    let last_backup_remainder = script.last_backup.map(|last_backup| {
        let (_, remainder) = round_duration(
            now - last_backup.min(now),
            RoundAccuracy::Minutes,
            RoundDirection::Up,
        );
        remainder
    });

    let remainder =
        last_backup_remainder.map_or(next_backup_remainder, |r| r.min(next_backup_remainder));

    Utc::now() + remainder
}

fn next_reminder(script: &Script) -> Option<DateTime<Utc>> {
    let reminder = script.reminder?;
    let next_reminder = script
        .last_backup
        .as_ref()
        .map_or_else(Utc::now, |last_backup| *last_backup + reminder);
    Some(next_reminder)
}

fn tooltip(script: &Script, state: &ScriptState) -> String {
    let last_backup = if let Some(last_backup) = script.last_backup {
        let now = Utc::now();
        let (last_backup, _) = round_duration(
            now - last_backup.min(now),
            RoundAccuracy::Minutes,
            RoundDirection::Down,
        );
        format!(
            "Last backup was {} ago",
            humantime::format_duration(last_backup.to_std().unwrap())
        )
    } else {
        "Never backed up before".to_string()
    };

    let status = match state {
        ScriptState::WaitingForTime => {
            let now = Utc::now();
            let (next_backup, _) = round_duration(
                next_backup(script).max(now) - now,
                RoundAccuracy::Minutes,
                RoundDirection::Down,
            );
            format!(
                "Next backup in {}",
                humantime::format_duration(next_backup.to_std().unwrap())
            )
        }
        ScriptState::WaitingForPath(_) => {
            format!(
                "Waiting for backup folder \"{}\" to appear",
                script.backup_path.display()
            )
        }
        ScriptState::Running => "Running".to_string(),
        ScriptState::Failed(_, message) => format!("Failed: {message}",),
    };

    format!("{last_backup}\n{status}")
}

fn is_mounted(path: &Path) -> io::Result<bool> {
    log::debug!("checking if `{}` exists and is writable", path.display());

    if path.exists() {
        let test_file_path = path.join(".backup-monitor-test");

        match File::create_new(&test_file_path) {
            Ok(_) => {
                fs::remove_file(&test_file_path)?;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_duration() {
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
                duration("5h 17s 389us"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (duration("5h"), duration("17s 389us"))
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
                duration("34s 94us"),
                RoundAccuracy::Minutes,
                RoundDirection::Down
            ),
            (Duration::zero(), duration("34s 94us"))
        );
    }
}
