use std::sync::{mpsc::RecvTimeoutError, Arc};

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

pub fn main_loop(
    clock: Clock,
    settings: Arc<ArcSwap<Settings>>,
    rx: impl ReceiveEvent,
    handle: impl TrayHandle<Tray>,
    autolaunch: AutoLaunch,
) -> anyhow::Result<()> {
    let mut manager = ScriptManager::new(clock, settings.clone());

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

        let (tray_data, show_reminder, next_wakeup) =
            analyze(&mut manager, &clock, &mut last_reminder, &settings.load())?;

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

        handle_event(event, &settings, &mut manager, &handle)?;
    }
}

fn handle_event(
    event: Option<Event>,
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
        Some(Event::MountDetected) | None => {
            log::info!("running scripts");

            manager.run(None, handle)?;
        }
    }
    Ok(())
}

fn wait(
    next_wakeup: Option<DateTime<Utc>>,
    clock: &Clock,
    rx: &impl ReceiveEvent,
) -> anyhow::Result<Option<Event>> {
    let timeout = match next_wakeup {
        Some(deadline) => {
            let now = clock.now();
            let deadline = deadline.max(now);
            let timeout = (deadline - now).to_std()?;
            log::trace!(
                "waiting until {} ({})",
                deadline.with_timezone(&Local),
                humantime::format_duration(timeout)
            );
            Some(timeout)
        }
        None => {
            log::trace!("waiting");
            None
        }
    };

    match rx.recv_timeout(timeout) {
        Ok(event) => Ok(Some(event)),
        Err(RecvTimeoutError::Timeout) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn analyze(
    manager: &mut impl Manager,
    clock: &Clock,
    last_reminder: &mut Option<DateTime<Utc>>,
    settings: &Settings,
) -> anyhow::Result<(TrayData, bool, Option<DateTime<Utc>>)> {
    let mut show_reminder = false;

    let next_backup = manager.next_backup();
    let next_reminder = manager.next_reminder();
    let next_ui_update = manager.next_ui_update();

    let next_reminder_notification = next_reminder_notification(next_reminder, last_reminder);

    let next_wakeup = next_wakeup(next_backup, next_reminder_notification, next_ui_update);

    if next_reminder_notification.is_some_and(|ts| ts <= clock.now())
        && last_reminder.map_or(true, |ts| ts <= clock.now() - REMINDER_INTERVAL)
    {
        show_reminder = true;
        *last_reminder = Some(clock.now());
    }

    let tray_data = TrayData {
        status: if next_reminder.is_some_and(|ts| ts <= clock.now()) {
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
) -> Option<DateTime<Utc>> {
    let mut next_wakeup = next_backup;

    if let Some(next_reminder) = next_reminder_notification {
        if next_wakeup.map_or(true, |ts| ts >= next_reminder) {
            next_wakeup = Some(next_reminder);
        }
    }

    if let Some(next_ui_update) = next_ui_update {
        if next_wakeup.map_or(true, |ts| ts >= next_ui_update) {
            next_wakeup = Some(next_ui_update);
        }
    }

    next_wakeup
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_manager::MockManager;
    use fake::{Fake, Faker};
    use serde::Deserialize;
    use std::{fs::File, time::Duration};

    #[derive(Debug, Deserialize)]
    struct AnalyzeTestCase {
        #[serde(default, with = "humantime_serde")]
        next_backup: Option<Duration>,

        #[serde(default, with = "humantime_serde")]
        next_reminder: Option<Duration>,

        #[serde(default, with = "humantime_serde")]
        next_ui_update: Option<Duration>,

        // scripts: Vec<Script>,
        #[serde(default, with = "humantime_serde")]
        last_reminder: Option<Duration>,

        tray_data: TrayData,

        show_reminder: bool,

        #[serde(default, with = "humantime_serde")]
        next_wakeup: Option<Duration>,
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
            analyze(&mut manager, &clock, &mut last_reminder, &settings).unwrap();

        assert_eq!(
            (
                tray_data,
                show_reminder,
                next_wakeup.map(|ts| (ts - clock.now()).to_std().unwrap())
            ),
            (
                test_case.tray_data,
                test_case.show_reminder,
                test_case.next_wakeup
            ),
            "{name}"
        );
    }
}
