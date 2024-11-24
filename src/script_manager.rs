use std::{
    collections::{HashMap, HashSet},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::Instant,
};

use arc_swap::ArcSwap;
use chrono::{DateTime, Duration, Utc};
use itertools::Itertools;
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
    WaitingForPaths(Vec<PathBuf>),
    Running,
    Failed(DateTime<Utc>, String),
}

pub struct ScriptManager {
    clock: Clock,
    settings: Arc<ArcSwap<Settings>>,
    states: HashMap<String, ScriptState>,
    mounts: HashSet<PathBuf>,
}

impl ScriptManager {
    pub fn new(clock: Clock, settings: Arc<ArcSwap<Settings>>, mounts: &str) -> ScriptManager {
        ScriptManager {
            clock,
            settings,
            states: HashMap::new(),
            mounts: parse_mounts(mounts),
        }
    }

    fn script_state(&self, script: &Script) -> ScriptState {
        match self.states.get(&script.name) {
            Some(ScriptState::WaitingForPaths(paths))
                if !script
                    .mount_paths
                    .iter()
                    .any(|backup_path| paths.contains(backup_path)) =>
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
        let now = self.clock.now();
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter_map(|script| match self.script_state(script) {
                ScriptState::WaitingForTime => Some(next_backup(now, script)),
                ScriptState::WaitingForPaths(_) | ScriptState::Running => None,
                ScriptState::Failed(ts, _) => Some(ts + RETRY_INTERVAL),
            })
            .min()
    }

    fn next_reminder(&self) -> Option<DateTime<Utc>> {
        let now = self.clock.now();
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter(|script| self.script_state(script) != ScriptState::Running)
            .filter_map(|script| next_reminder(now, script))
            .min()
    }

    fn next_ui_update(&self) -> Option<DateTime<Utc>> {
        let now = self.clock.now();
        let settings = self.settings.load();

        settings
            .scripts
            .iter()
            .filter(|script| self.script_state(script) == ScriptState::WaitingForTime)
            .map(|script| next_ui_update(now, script))
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

    fn set_mounts(&mut self, mounts: &str) {
        let mounts = parse_mounts(mounts);

        for mount in &self.mounts {
            if !mounts.contains(mount) {
                log::debug!("`{}` has been unmounted", mount.display());
            }
        }

        for mount in &mounts {
            if !self.mounts.contains(mount) {
                log::debug!("`{}` has been mounted", mount.display());
            }
        }

        self.mounts = mounts;
    }

    fn run(
        &mut self,
        script_name: Option<&str>,
        handle: &impl TrayHandle<Tray>,
    ) -> anyhow::Result<()> {
        let settings = self.settings.load_full();

        for script in &settings.scripts {
            let now = self.clock.now();

            if script_name.is_some_and(|name| name == script.name)
                || (script_name.is_none() && next_backup(now, script) <= now)
            {
                if script
                    .mount_paths
                    .iter()
                    .all(|path| self.mounts.contains(path))
                {
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
                    let paths = script
                        .mount_paths
                        .iter()
                        .filter(|path| !self.mounts.contains(*path))
                        .cloned()
                        .collect::<Vec<_>>();

                    log::debug!(
                        "waiting for folders {} to be mounted",
                        paths
                            .iter()
                            .map(|path| format!("`{}`", path.display()))
                            .join(", ")
                    );

                    self.states
                        .insert(script.name.clone(), ScriptState::WaitingForPaths(paths));
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

fn parse_mounts(mounts: &str) -> HashSet<PathBuf> {
    mounts
        .lines()
        .filter_map(
            |line| match line.split_whitespace().collect::<Vec<_>>()[..] {
                [_, mount, ..] => Some(PathBuf::from(mount)),
                _ => None,
            },
        )
        .collect()
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

fn next_backup(now: DateTime<Utc>, script: &Script) -> DateTime<Utc> {
    script
        .last_backup
        .as_ref()
        .map_or(now, |last_backup| *last_backup + script.interval)
}

fn next_ui_update(now: DateTime<Utc>, script: &Script) -> DateTime<Utc> {
    let (_, next_backup_remainder) = round_duration(
        next_backup(now, script).max(now) - now,
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

    now + remainder + Duration::milliseconds(1)
}

fn next_reminder(now: DateTime<Utc>, script: &Script) -> Option<DateTime<Utc>> {
    let reminder = script.reminder?;
    let next_reminder = script
        .last_backup
        .as_ref()
        .map_or(now, |last_backup| *last_backup + reminder);
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
                next_backup(now, script).max(now) - now,
                RoundAccuracy::Minutes,
                RoundDirection::Down,
            );
            format!(
                "Next backup in {}",
                humantime::format_duration(next_backup.to_std().unwrap())
            )
        }
        ScriptState::WaitingForPaths(paths) => {
            format!(
                "Waiting for folders {} to be mounted",
                paths
                    .iter()
                    .map(|path| format!("\"{}\"", path.display()))
                    .join(", ")
            )
        }
        ScriptState::Running => "Running".to_string(),
        ScriptState::Failed(_, message) => format!("Failed: {message}",),
    };

    format!("{last_backup}\n{status}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fake::{Fake, Faker};
    use indoc::indoc;
    use serde::Deserialize;
    use std::{cmp::max, fs::File, time::Duration};

    #[derive(Debug, Deserialize)]
    struct ScheduleTestScript {
        pub mount_paths: Vec<PathBuf>,

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
                mount_paths: self.mount_paths,
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
    #[case("blocked_by_last_backup_3")]
    #[case("waiting_for_path")]
    #[case("running")]
    #[case("failed_with_cooldown")]
    #[case("failed_without_cooldown")]
    fn schedule(#[case] name: &str) {
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
        let now = clock.now();
        let settings = Arc::new(ArcSwap::from_pointee(Settings {
            scripts: test_case
                .scripts
                .into_iter()
                .map(|script| script.into_script(&clock))
                .collect(),
            ..Default::default()
        }));
        let mut manager = ScriptManager::new(clock, settings.clone(), "");

        for (script, state) in settings.load().scripts.iter().zip(script_states) {
            if let Some(state) = state {
                let state = match state.split(':').collect::<Vec<_>>()[..] {
                    ["WaitingForTime"] => ScriptState::WaitingForTime,
                    ["WaitingForPath", path] => ScriptState::WaitingForPaths(vec![path.into()]),
                    ["Running"] => ScriptState::Running,
                    ["Failed", ts, message] => ScriptState::Failed(
                        now - humantime::parse_duration(ts).unwrap(),
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
                    .map(|ts| (max(ts, now) - now).to_std().unwrap()),
                manager
                    .next_reminder()
                    .map(|ts| (max(ts, now) - now).to_std().unwrap()),
                manager
                    .next_ui_update()
                    .map(|ts| (max(ts, now) - now).to_std().unwrap()),
            ),
            (
                test_case.next_backup,
                test_case.next_reminder,
                test_case.next_ui_update
            ),
            "{name}"
        );
    }

    #[test]
    fn set_mounts() {
        let clock = Faker.fake::<Clock>();
        let settings = Arc::new(ArcSwap::from_pointee(Settings::default()));
        let mut manager = ScriptManager::new(clock, settings, "");

        manager.set_mounts(indoc! {"
            /dev/nvme0n1p2 / btrfs rw,relatime,ssd,discard=async,space_cache=v2,subvolid=403,subvol=/@/.snapshots/138/snapshot 0 0
            devtmpfs /dev devtmpfs rw,nosuid,size=4096k,nr_inodes=8192558,mode=755,inode64 0 0
            tmpfs /dev/shm tmpfs rw,nosuid,nodev,inode64 0 0
        "});

        assert!(manager.mounts.contains(&PathBuf::from("/")));
        assert!(manager.mounts.contains(&PathBuf::from("/dev/shm")));
        assert!(!manager.mounts.contains(&PathBuf::from("/does-not-exist")));
    }
}
