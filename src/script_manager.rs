use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Instant,
};

use arc_swap::ArcSwap;
use chrono::{DateTime, Duration, Utc};
use notify_rust::{Hint, Notification, Timeout};
use serde::Deserialize;
use tempfile::NamedTempFile;

use crate::tray_handle::TrayHandle;
use crate::{clock::Clock, manager::Manager};
use crate::{
    round_duration::{round_duration, RoundAccuracy, RoundDirection},
    tray_handle::TrayData,
};
use crate::{
    settings::{Script, Settings},
    tray::Tray,
    RETRY_INTERVAL,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
enum ScriptState {
    WaitingForTime,
    WaitingForPath(PathBuf),
    Running,
    Failed(DateTime<Utc>, String),
}

pub struct ScriptManager {
    clock: Clock,
    settings: Arc<ArcSwap<Settings>>,
    states: HashMap<String, ScriptState>,
}

impl ScriptManager {
    pub fn new(clock: Clock, settings: Arc<ArcSwap<Settings>>) -> ScriptManager {
        ScriptManager {
            clock,
            settings,
            states: HashMap::new(),
        }
    }

    fn script_state(&self, script: &Script) -> ScriptState {
        match self.states.get(&script.name) {
            Some(ScriptState::WaitingForPath(path))
                if script
                    .backup_path
                    .as_ref()
                    .map_or(true, |backup_path| path != backup_path) =>
            {
                ScriptState::WaitingForTime
            }
            Some(state) => state.clone(),
            None => ScriptState::WaitingForTime,
        }
    }
}

impl Manager for ScriptManager {
    fn next_backup(&self) -> Option<DateTime<Utc>> {
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter_map(|script| match self.script_state(script) {
                ScriptState::WaitingForTime => Some(next_backup(&self.clock, script)),
                ScriptState::WaitingForPath(_) | ScriptState::Running => None,
                ScriptState::Failed(ts, _) => Some(ts + RETRY_INTERVAL),
            })
            .min()
    }

    fn next_reminder(&self) -> Option<DateTime<Utc>> {
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter(|script| self.script_state(script) != ScriptState::Running)
            .filter_map(|script| next_reminder(&self.clock, script))
            .min()
    }

    fn next_ui_update(&self) -> Option<DateTime<Utc>> {
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter(|script| self.script_state(script) == ScriptState::WaitingForTime)
            .map(|script| next_ui_update(&self.clock, script))
            .min()
    }

    fn tooltip(&self) -> String {
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
                        &self.clock,
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

    fn run(
        &mut self,
        script_name: Option<&str>,
        handle: &impl TrayHandle<Tray>,
    ) -> anyhow::Result<()> {
        let settings = self.settings.load_full();

        for script in &settings.scripts {
            if script_name.is_some_and(|name| name == script.name)
                || (script_name.is_none() && next_backup(&self.clock, script) <= self.clock.now())
            {
                if script.backup_path.as_ref().map_or(Ok(true), is_mounted)? {
                    log::info!("running backup script `{}`", script.name);

                    self.states
                        .insert(script.name.clone(), ScriptState::Running);

                    let mut notification_handle = Notification::new()
                        .appname(&settings.title)
                        .summary(&format!("Running {}", script.name))
                        .icon(&settings.icon_name)
                        .hint(Hint::Resident(true))
                        .timeout(Timeout::Never)
                        .show()?;

                    handle.update(TrayData {
                        status: Some(ksni::Status::Active),
                        tooltip: Some(self.tooltip()),
                        ..Default::default()
                    });

                    let tmp = write_script(&script.backup_script)?;

                    let start = Instant::now();

                    let state;
                    let summary;
                    let body;
                    match Command::new(tmp.path()).status() {
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
                                    script.last_backup = Some(self.clock.now());
                                }

                                // save new settings
                                settings.save()?;
                            } else if let Some(code) = status.code() {
                                summary = format!("{} failed with exit code {code}", script.name);
                                body = String::new();
                                state = ScriptState::Failed(self.clock.now(), summary.clone());
                            } else {
                                summary = format!("{} failed", script.name);
                                body = String::new();
                                state = ScriptState::Failed(self.clock.now(), summary.clone());
                            }
                        }
                        Err(error) => {
                            summary = format!("{} failed with error", script.name);
                            body = error.to_string();
                            state = ScriptState::Failed(self.clock.now(), error.to_string());
                        }
                    };

                    self.states.insert(script.name.clone(), state);

                    for action in &script.post_backup_actions {
                        notification_handle.action(&action.label, &action.label);
                    }
                    notification_handle.summary(&summary);
                    notification_handle.body(&body);
                    notification_handle.timeout(Timeout::Milliseconds(6_000));
                    notification_handle.update();
                    notification_handle.wait_for_action(|action_label| {
                        if let Some(action) = script
                            .post_backup_actions
                            .iter()
                            .find(|action| action.label == action_label)
                        {
                            log::info!("running post backup script `{}`", action.label);

                            let tmp = write_script(&action.script).unwrap();

                            let summary;
                            let body;
                            match Command::new(tmp.path()).status() {
                                Ok(status) => {
                                    if status.success() {
                                        summary = format!("{} finished", action.label);
                                        body = String::new();
                                    } else {
                                        summary = format!("{} failed", action.label);
                                        body = String::new();
                                    }
                                }
                                Err(error) => {
                                    summary = format!("{} failed with error", action.label);
                                    body = error.to_string();
                                }
                            };

                            Notification::new()
                                .appname(&settings.title)
                                .summary(&summary)
                                .body(&body)
                                .icon(&settings.icon_name)
                                .timeout(Timeout::Milliseconds(6_000))
                                .show()
                                .unwrap();
                        }
                    });
                } else {
                    log::debug!(
                        "waiting for `{}` to appear",
                        script.backup_path.as_ref().unwrap().display()
                    );

                    self.states.insert(
                        script.name.clone(),
                        ScriptState::WaitingForPath(script.backup_path.as_ref().unwrap().clone()),
                    );
                }
            }

            handle.update(TrayData {
                tooltip: Some(self.tooltip()),
                ..Default::default()
            });
        }

        Ok(())
    }
}

fn write_script(script: &str) -> Result<NamedTempFile, anyhow::Error> {
    let mut tmp = NamedTempFile::new()?;
    tmp.write_all(script.as_bytes())?;

    let metadata = tmp.as_file().metadata()?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o700);
    tmp.as_file().set_permissions(permissions)?;

    Ok(tmp)
}

fn next_backup(clock: &Clock, script: &Script) -> DateTime<Utc> {
    script
        .last_backup
        .as_ref()
        .map_or_else(|| clock.now(), |last_backup| *last_backup + script.interval)
}

fn next_ui_update(clock: &Clock, script: &Script) -> DateTime<Utc> {
    let now = clock.now();

    let (_, next_backup_remainder) = round_duration(
        next_backup(clock, script).max(now) - now,
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

    clock.now() + remainder
}

fn next_reminder(clock: &Clock, script: &Script) -> Option<DateTime<Utc>> {
    let reminder = script.reminder?;
    let next_reminder = script
        .last_backup
        .as_ref()
        .map_or_else(|| clock.now(), |last_backup| *last_backup + reminder);
    Some(next_reminder)
}

fn tooltip(clock: &Clock, script: &Script, state: &ScriptState) -> String {
    let last_backup = if let Some(last_backup) = script.last_backup {
        let now = clock.now();
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
            let now = clock.now();
            let (next_backup, _) = round_duration(
                next_backup(clock, script).max(now) - now,
                RoundAccuracy::Minutes,
                RoundDirection::Down,
            );
            format!(
                "Next backup in {}",
                humantime::format_duration(next_backup.to_std().unwrap())
            )
        }
        ScriptState::WaitingForPath(path) => {
            format!("Waiting for backup folder \"{}\" to appear", path.display())
        }
        ScriptState::Running => "Running".to_string(),
        ScriptState::Failed(_, message) => format!("Failed: {message}",),
    };

    format!("{last_backup}\n{status}")
}

fn is_mounted(path: impl AsRef<Path>) -> io::Result<bool> {
    let path = path.as_ref();

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
    use fake::{Fake, Faker};
    use serde::Deserialize;
    use std::{fs::File, time::Duration};

    #[derive(Debug, Deserialize)]
    struct ScheduleTestScript {
        pub backup_path: Option<PathBuf>,

        #[serde(with = "humantime_serde")]
        pub interval: Duration,

        #[serde(default, with = "humantime_serde")]
        pub reminder: Option<Duration>,

        #[serde(default, with = "humantime_serde")]
        pub last_backup: Option<Duration>,

        pub state: Option<String>,
    }

    impl ScheduleTestScript {
        fn into_script(self, clock: &Clock) -> Script {
            Script {
                name: Faker.fake(),
                icon_name: None,
                backup_script: "#!/bin/bash".to_string(),
                backup_path: self.backup_path,
                interval: self.interval,
                reminder: self.reminder,
                post_backup_actions: Vec::new(),
                last_backup: self.last_backup.map(|delta| clock.now() - delta),
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct ScheduleTestCase {
        scripts: Vec<ScheduleTestScript>,

        #[serde(default, with = "humantime_serde")]
        next_backup: Option<Duration>,

        #[serde(default, with = "humantime_serde")]
        next_reminder: Option<Duration>,

        #[serde(default, with = "humantime_serde")]
        next_ui_update: Option<Duration>,
    }

    #[rstest::rstest]
    #[case("empty")]
    #[case("never_backed_up")]
    #[case("blocked_by_last_backup_1")]
    #[case("blocked_by_last_backup_2")]
    #[case("waiting_for_path")]
    #[case("running")]
    #[case("failed_with_cooldown")]
    #[case("failed_without_cooldown")]
    fn schedule(#[case] name: &str) {
        use std::cmp::max;

        let test_case = serde_hjson::from_reader::<_, ScheduleTestCase>(
            File::open(format!("./src/test_cases/manager/{name}.hjson")).unwrap(),
        )
        .unwrap();

        let script_states = test_case
            .scripts
            .iter()
            .map(|script| &script.state)
            .cloned()
            .collect::<Vec<_>>();

        let clock = Faker.fake::<Clock>();
        let settings = Arc::new(ArcSwap::from_pointee(Settings {
            scripts: test_case
                .scripts
                .into_iter()
                .map(|script| script.into_script(&clock))
                .collect(),
            ..Default::default()
        }));
        let mut manager = ScriptManager::new(clock, settings.clone());

        for (script, state) in settings.load().scripts.iter().zip(script_states) {
            if let Some(state) = state {
                let state = match state.split(':').collect::<Vec<_>>()[..] {
                    ["WaitingForTime"] => ScriptState::WaitingForTime,
                    ["WaitingForPath", path] => ScriptState::WaitingForPath(path.into()),
                    ["Running"] => ScriptState::Running,
                    ["Failed", ts, message] => ScriptState::Failed(
                        clock.now() - humantime::parse_duration(ts).unwrap(),
                        message.to_string(),
                    ),
                    _ => unimplemented!(),
                };
                manager.states.insert(script.name.clone(), state);
            }
        }

        assert_eq!(
            (
                manager
                    .next_backup()
                    .map(|ts| (max(ts, clock.now()) - clock.now()).to_std().unwrap()),
                manager
                    .next_reminder()
                    .map(|ts| (max(ts, clock.now()) - clock.now()).to_std().unwrap()),
                manager
                    .next_ui_update()
                    .map(|ts| (max(ts, clock.now()) - clock.now()).to_std().unwrap()),
            ),
            (
                test_case.next_backup,
                test_case.next_reminder,
                test_case.next_ui_update
            ),
            "{name}"
        );
    }
}
