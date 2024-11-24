use chrono::{DateTime, Utc};

use crate::manager::Manager;
use crate::tray::Tray;
use crate::tray_handle::TrayHandle;

#[derive(Debug, Default)]
pub struct MockManager {
    pub next_backup: Option<DateTime<Utc>>,
    pub next_reminder: Option<DateTime<Utc>>,
    pub next_ui_update: Option<DateTime<Utc>>,
    pub tooltip: String,
    pub run: Vec<Option<String>>,
}

impl Manager for MockManager {
    fn next_backup(&self) -> Option<DateTime<Utc>> {
        self.next_backup
    }

    fn next_reminder(&self) -> Option<DateTime<Utc>> {
        self.next_reminder
    }

    fn next_ui_update(&self) -> Option<DateTime<Utc>> {
        self.next_ui_update
    }

    fn tooltip(&self) -> String {
        self.tooltip.clone()
    }

    fn set_mounts(&mut self, _mounts: &str) {}

    fn run(
        &mut self,
        script_name: Option<&str>,
        _handle: &impl TrayHandle<Tray>,
    ) -> anyhow::Result<()> {
        self.run.push(script_name.map(ToString::to_string));
        Ok(())
    }
}
