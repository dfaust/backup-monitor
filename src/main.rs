use std::{
    env::current_exe,
    fs::File,
    io,
    os::unix::prelude::AsRawFd,
    sync::{
        mpsc::{self, RecvTimeoutError, Sender},
        Arc,
    },
    thread,
};

use anyhow::bail;
use arc_swap::ArcSwap;
use auto_launch::AutoLaunchBuilder;
use chrono::{Duration, Local, Utc};
use env_logger::Env;
use mio::{unix::SourceFd, Events, Interest, Poll, Token};
use notify::Watcher;
use notify_rust::{Notification, Timeout};

mod manager;
mod settings;
mod tray;

use manager::Manager;
use settings::{settings_file_path, Settings};
use tray::Tray;

pub const RETRY_INTERVAL: Duration = Duration::hours(1);
pub const REMINDER_INTERVAL: Duration = Duration::hours(4);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    MountDetected,
    SettingsChanged,
    ManualRun(String),
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("debug")).init();

    let settings = Settings::load()?;

    let (tx, rx) = mpsc::channel::<Event>();

    let tx_tray = tx.clone();
    let service = ksni::TrayService::new(Tray::new(&settings, tx_tray));
    let handle = service.handle();
    service.spawn();

    // watch for mounts
    let tx_mounts = tx.clone();
    thread::spawn(|| {
        let file = File::open("/proc/mounts").unwrap();
        poll_mounts(file, tx_mounts)
    });

    // watch for changes to settings file
    let tx_settings = tx.clone();
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| match res {
            Ok(event) => {
                log::debug!("fs event: {event:?}");
                if matches!(
                    event.kind,
                    notify::event::EventKind::Modify(notify::event::ModifyKind::Data(_))
                ) {
                    let _ = tx_settings.send(Event::SettingsChanged);
                }
            }
            Err(e) => eprintln!("watch error: {e:?}"),
        })?;

    let settings_file_path = settings_file_path()?;
    watcher.watch(&settings_file_path, notify::RecursiveMode::NonRecursive)?;

    // autostart
    let current_exe = current_exe()?;
    let autolaunch = AutoLaunchBuilder::new()
        .set_app_name("Backup Monitor")
        .set_app_path(&current_exe.display().to_string())
        .set_use_launch_agent(true)
        .build()?;

    let settings = Arc::new(ArcSwap::from_pointee(settings));

    let mut manager = Manager::new(settings.clone());

    let mut last_reminder = None;

    loop {
        let next_backup = manager.next_backup();
        let next_reminder = manager.next_reminder();
        let next_ui_update = manager.next_ui_update();

        // limit reminder notifications frequency
        let next_reminder_notification = next_reminder.map(|next| {
            if next <= Utc::now() {
                next.max(
                    last_reminder
                        .map(|last| last + REMINDER_INTERVAL)
                        .unwrap_or(next),
                )
            } else {
                next
            }
        });

        // dbg!(
        //     next_backup.map(|ts| ts - Utc::now()),
        //     next_reminder.map(|ts| ts - Utc::now()),
        //     next_reminder_notification.map(|ts| ts - Utc::now()),
        //     next_ui_update.map(|ts| ts - Utc::now()),
        // );

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

        if next_reminder_notification.is_some_and(|ts| ts <= Utc::now())
            && last_reminder.map_or(true, |ts| ts <= Utc::now() - REMINDER_INTERVAL)
            && next_backup != next_wakeup
        {
            let settings = settings.load();
            Notification::new()
                .appname(&settings.title)
                .summary("Backup out of date")
                .body("Make sure to run backups regularly")
                .icon(&settings.icon_name)
                .timeout(Timeout::Milliseconds(10_000))
                .show()?;
            last_reminder = Some(Utc::now());
        }

        handle.update(|tray| {
            if next_reminder.is_some_and(|ts| ts <= Utc::now()) {
                tray.set_status(ksni::Status::NeedsAttention);
            } else {
                tray.set_status(ksni::Status::Passive);
            }

            tray.set_tooltip(manager.tooltip());

            let settings = settings.load();
            let scripts = settings
                .scripts
                .iter()
                .map(|script| (script.name.clone(), script.icon_name.clone()))
                .collect();
            tray.set_scripts(scripts);
        });

        if autolaunch.is_enabled()? != settings.load().autostart {
            if autolaunch.is_enabled()? {
                log::info!("disabling autostart");
                autolaunch.disable()?;
            } else {
                log::info!("enabling autostart");
                autolaunch.enable()?;
            }
        }

        let event = match next_wakeup {
            Some(deadline) => {
                let now = Utc::now();
                let deadline = deadline.max(now);
                let timeout = (deadline - now).to_std()?;
                log::debug!(
                    "waiting until {} ({})",
                    deadline.with_timezone(&Local),
                    humantime::format_duration(timeout)
                );
                match rx.recv_timeout(timeout) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(error) => bail!(error),
                }
            }
            None => {
                log::debug!("waiting");
                match rx.recv() {
                    Ok(event) => Some(event),
                    Err(error) => bail!(error),
                }
            }
        };

        match event {
            Some(Event::SettingsChanged) => {
                log::info!("reloading settings");

                let loaded_settings = Settings::load()?;

                settings.store(Arc::new(loaded_settings));
            }
            Some(Event::ManualRun(name)) => {
                log::info!("running script {name}");

                manager.run(Some(&name), &handle)?;
            }
            _ => {
                log::info!("running scripts");

                manager.run(None, &handle)?;
            }
        }
    }
}

fn poll_mounts(file: File, tx: Sender<Event>) -> io::Result<()> {
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(1024);

    poll.registry().register(
        &mut SourceFd(&file.as_raw_fd()),
        Token(0),
        Interest::READABLE,
    )?;

    loop {
        poll.poll(&mut events, None)?;

        log::debug!("mounts were updated");

        let _ = tx.send(Event::MountDetected);
    }
}
