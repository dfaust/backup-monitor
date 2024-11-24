use chrono::{DateTime, Utc};

use crate::{tray::Tray, tray_handle::TrayHandle};

pub trait Manager {
    fn next_backup(&self) -> Option<DateTime<Utc>>;

    fn next_reminder(&self) -> Option<DateTime<Utc>>;

    fn next_ui_update(&self) -> Option<DateTime<Utc>>;

    fn tooltip(&self) -> String;

    fn run<'a>(
        &'a mut self,
        script_name: Option<&'a str>,
        handle: &impl TrayHandle<Tray>,
    ) -> anyhow::Result<()>;
}
