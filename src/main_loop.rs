use std::{
    fmt,
    sync::{mpsc::RecvTimeoutError, Arc},
};

use arc_swap::ArcSwap;
use auto_launch::AutoLaunch;
use chrono::{DateTime, Local, Utc};
use notify_rust::{Notification, Timeout};

use crate::{
    clock::Clock,
    event::ReceiveEvent,
    manager::Manager,
    script_manager::ScriptManager,
    settings::Settings,
    tray::Tray,
    tray_handle::{TrayData, TrayHandle},
    Event, REMINDER_INTERVAL,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeupReason {
    RunScripts,
    ShowReminder,
    UpdateUi,
}

impl fmt::Display for WakeupReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                WakeupReason::RunScripts => "run scripts",
                WakeupReason::ShowReminder => "show reminder",
                WakeupReason::UpdateUi => "update ui",
            }
        )
    }
}

pub fn main_loop(
    clock: Clock,
    settings: Arc<ArcSwap<Settings>>,
    mounts: String,
    rx: impl ReceiveEvent,
    handle: impl TrayHandle<Tray>,
    autolaunch: AutoLaunch,
) -> anyhow::Result<()> {
    let mut manager = ScriptManager::new(clock, settings.clone(), &mounts);

    let mut last_reminder = None;

    loop {
        if autolaunch.is_enabled()? != settings.load().autostart {
            if autolaunch.is_enabled()? {
                log::info!("disabling autostart");
                autolaunch.disable()?;
            } else {
                log::info!("enabling autostart");
                autolaunch.enable()?;
            }
        }

        let (tray_data, show_reminder, next_wakeup) = analyze(
            clock.now(),
            &mut manager,
            &mut last_reminder,
            &settings.load(),
        )?;

        handle.update(tray_data);

        if show_reminder {
            let settings = settings.load();
            Notification::new()
                .appname(&settings.title)
                .summary("Backup out of date")
                .body("Make sure to run backups regularly")
                .icon(&settings.icon_name)
                .timeout(Timeout::Milliseconds(10_000))
                .show()?;
        }

        let event = wait(next_wakeup, &clock, &rx)?;

        handle_event(event, next_wakeup, &settings, &mut manager, &handle)?;
    }
}

fn handle_event(
    event: Option<Event>,
    next_wakeup: Option<(DateTime<Utc>, WakeupReason)>,
    settings: &Arc<ArcSwap<Settings>>,
    manager: &mut impl Manager,
    handle: &impl TrayHandle<Tray>,
) -> anyhow::Result<()> {
    match event {
        Some(Event::SettingsChanged) => {
            log::info!("reloading settings");

            let loaded_settings = Settings::load()?;

            settings.store(Arc::new(loaded_settings));
        }
        Some(Event::ManualRun(name)) => {
            log::info!("running script {name}");

            manager.run(Some(&name), handle)?;
        }
        Some(Event::MountsChanged(mounts)) => {
            log::info!("reloading mounts");

            manager.set_mounts(&mounts);

            log::info!("running scripts");

            manager.run(None, handle)?;
        }
        None if next_wakeup.is_none_or(|(_, reason)| reason == WakeupReason::RunScripts) => {
            log::info!("running scripts");

            manager.run(None, handle)?;
        }
        None => {}
    }
    Ok(())
}

fn wait(
    next_wakeup: Option<(DateTime<Utc>, WakeupReason)>,
    clock: &Clock,
    rx: &impl ReceiveEvent,
) -> anyhow::Result<Option<Event>> {
    let timeout = match next_wakeup {
        Some((deadline, reason)) => {
            let now = clock.now();
            let deadline = deadline.max(now);
            let timeout = (deadline - now).to_std()?;
            log::debug!(
                "waiting until {} ({}) to {reason}",
                deadline.with_timezone(&Local),
                humantime::format_duration(timeout)
            );
            Some(timeout)
        }
        None => {
            log::debug!("waiting");
            None
        }
    };

    match rx.recv_timeout(timeout) {
        Ok(event) => Ok(Some(event)),
        Err(RecvTimeoutError::Timeout) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[allow(clippy::type_complexity)]
fn analyze(
    now: DateTime<Utc>,
    manager: &mut impl Manager,
    last_reminder: &mut Option<DateTime<Utc>>,
    settings: &Settings,
) -> anyhow::Result<(TrayData, bool, Option<(DateTime<Utc>, WakeupReason)>)> {
    let mut show_reminder = false;

    let next_backup = manager.next_backup();
    let next_reminder = manager.next_reminder();
    let next_ui_update = manager.next_ui_update();

    let next_reminder_notification = next_reminder_notification(next_reminder, last_reminder);

    let next_wakeup = next_wakeup(next_backup, next_reminder_notification, next_ui_update);

    if next_reminder_notification.is_some_and(|ts| ts <= now)
        && last_reminder.map_or(true, |ts| ts <= now - REMINDER_INTERVAL)
    {
        show_reminder = true;
        *last_reminder = Some(now);
    }

    let tray_data = TrayData {
        status: if next_reminder.is_some_and(|ts| ts <= now) {
            Some(ksni::Status::NeedsAttention)
        } else {
            Some(ksni::Status::Passive)
        },
        tooltip: Some(manager.tooltip()),
        scripts: Some(
            settings
                .scripts
                .iter()
                .map(|script| (script.name.clone(), script.icon_name.clone()))
                .collect(),
        ),
    };

    Ok((tray_data, show_reminder, next_wakeup))
}

// limit reminder notifications frequency
fn next_reminder_notification(
    next_reminder: Option<DateTime<Utc>>,
    last_reminder: &Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    next_reminder.map(|next| {
        next.max(
            last_reminder
                .map(|last| last + REMINDER_INTERVAL)
                .unwrap_or(next),
        )
    })
}

fn next_wakeup(
    next_backup: Option<DateTime<Utc>>,
    next_reminder_notification: Option<DateTime<Utc>>,
    next_ui_update: Option<DateTime<Utc>>,
) -> Option<(DateTime<Utc>, WakeupReason)> {
    let mut next_wakeup = next_backup.map(|ts| (ts, WakeupReason::RunScripts));

    if let Some(next_reminder) = next_reminder_notification {
        if next_wakeup
            .as_ref()
            .map_or(true, |(ts, _)| *ts > next_reminder)
        {
            next_wakeup = Some((next_reminder, WakeupReason::ShowReminder));
        }
    }

    if let Some(next_ui_update) = next_ui_update {
        if next_wakeup
            .as_ref()
            .map_or(true, |(ts, _)| *ts > next_ui_update)
        {
            next_wakeup = Some((next_ui_update, WakeupReason::UpdateUi));
        }
    }

    next_wakeup
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_manager::MockManager;
    use fake::{Fake, Faker};
    use serde::{Deserialize, Deserializer};
    use std::{fs::File, time::Duration};

    pub fn deserialize_wakeup_reason<'de, D>(
        deserializer: D,
    ) -> Result<Option<WakeupReason>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            Some(s) => match s.to_lowercase().as_str() {
                "runscripts" => Ok(Some(WakeupReason::RunScripts)),
                "showreminder" => Ok(Some(WakeupReason::ShowReminder)),
                "updateui" => Ok(Some(WakeupReason::UpdateUi)),
                _ => Err(serde::de::Error::custom(format!(
                    "Invalid wakeup reason: {s}"
                ))),
            },
            None => Ok(None),
        }
    }

    #[derive(Debug, Deserialize)]
    struct AnalyzeTestCase {
        #[serde(default, with = "humantime_serde")]
        next_backup: Option<Duration>,

        #[serde(default, with = "humantime_serde")]
        next_reminder: Option<Duration>,

        #[serde(default, with = "humantime_serde")]
        next_ui_update: Option<Duration>,

        #[serde(default, with = "humantime_serde")]
        last_reminder: Option<Duration>,

        tray_data: TrayData,

        show_reminder: bool,

        #[serde(default, with = "humantime_serde")]
        next_wakeup: Option<Duration>,

        #[serde(default, deserialize_with = "deserialize_wakeup_reason")]
        wakeup_reason: Option<WakeupReason>,
    }

    #[rstest::rstest]
    #[case("waiting_for_path")]
    #[case("waiting_for_time")]
    #[case("next_ui_update")]
    #[case("next_ui_update_with_blocked_reminder")]
    #[case("next_reminder_now")]
    #[case("next_reminder_sleep")]
    #[case("next_reminder_with_last_reminder_blocking")]
    #[case("next_reminder_with_last_reminder_blocking_schedule")]
    #[case("next_reminder_with_last_reminder_expired")]
    #[case("next_reminder_with_last_reminder_expired_schedule")]
    fn analyze_test_cases(#[case] name: &str) {
        let test_case = serde_hjson::from_reader::<_, AnalyzeTestCase>(
            File::open(format!("./src/test_cases/main_loop/{name}.hjson")).unwrap(),
        )
        .unwrap();

        let clock = Faker.fake::<Clock>();
        let settings = Settings::default();
        let mut manager = MockManager {
            next_backup: test_case.next_backup.map(|delta| clock.now() + delta),
            next_reminder: test_case.next_reminder.map(|delta| clock.now() + delta),
            next_ui_update: test_case.next_ui_update.map(|delta| clock.now() + delta),
            ..Default::default()
        };
        let mut last_reminder = test_case.last_reminder.map(|delta| clock.now() - delta);

        let (tray_data, show_reminder, next_wakeup) =
            analyze(clock.now(), &mut manager, &mut last_reminder, &settings).unwrap();

        assert_eq!(
            (
                tray_data,
                show_reminder,
                next_wakeup.map(|(ts, _)| (ts - clock.now()).to_std().unwrap()),
                next_wakeup.map(|(_, reason)| reason)
            ),
            (
                test_case.tray_data,
                test_case.show_reminder,
                test_case.next_wakeup,
                test_case.wakeup_reason
            ),
            "{name}"
        );
    }
}
