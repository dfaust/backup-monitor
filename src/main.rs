use std::{
    env::current_exe,
    fs::File,
    io::{self, Read, Seek},
    os::unix::prelude::AsRawFd,
    sync::{
        mpsc::{self, Sender},
        Arc,
    },
    thread,
};

use arc_swap::ArcSwap;
use auto_launch::AutoLaunchBuilder;
use chrono::Duration;
use clock::Clock;
use env_logger::Env;
use event::{Event, EventReceiver};
use main_loop::main_loop;
use mio::{unix::SourceFd, Events, Interest, Poll, Token};
use notify::Watcher;

mod clock;
mod event;
mod main_loop;
mod manager;
mod mock_manager;
mod round_duration;
mod script_manager;
mod settings;
mod tray;
mod tray_handle;

use settings::{settings_file_path, Settings};
use tray::Tray;

pub const RETRY_INTERVAL: Duration = Duration::hours(1);
pub const REMINDER_INTERVAL: Duration = Duration::hours(4);

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("debug"))
        .format_timestamp(None)
        .init();

    let settings = Settings::load()?;

    let (tx, rx) = mpsc::channel::<Event>();
    let rx = EventReceiver::new(rx);

    let tx_tray = tx.clone();
    let service = ksni::TrayService::new(Tray::new(&settings, tx_tray));
    let handle = service.handle();
    service.spawn();

    // watch for mounts
    let mut file = File::open("/proc/mounts").unwrap();
    let mut mounts = String::new();
    let _ = file.read_to_string(&mut mounts);
    let tx_mounts = tx.clone();
    thread::spawn(|| poll_mounts(file, tx_mounts));

    // watch for changes to settings file
    let tx_settings = tx.clone();
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| match res {
            Ok(event) => {
                if matches!(
                    event.kind,
                    notify::event::EventKind::Modify(notify::event::ModifyKind::Data(_))
                ) {
                    log::debug!("settings have changed");

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
        .set_app_name("backup-monitor")
        .set_app_path(&current_exe.display().to_string())
        .build()?;

    let clock = Clock::new();
    let settings = Arc::new(ArcSwap::from_pointee(settings));

    main_loop(clock, settings, mounts, rx, handle, autolaunch)
}

fn poll_mounts(mut file: File, tx: Sender<Event>) -> io::Result<()> {
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(1024);

    poll.registry().register(
        &mut SourceFd(&file.as_raw_fd()),
        Token(0),
        Interest::READABLE,
    )?;

    loop {
        poll.poll(&mut events, None)?;

        log::debug!("mounts have changed");

        let mut mounts = String::new();
        let _ = file.rewind();
        let _ = file.read_to_string(&mut mounts);

        let _ = tx.send(Event::MountsChanged(mounts));
    }
}
