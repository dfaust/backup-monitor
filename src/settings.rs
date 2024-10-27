use std::{collections::HashSet, fs::File, io::Write, path::PathBuf, time::Duration};

use anyhow::{ensure, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PostScriptAction {
    pub label: String,

    pub script: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Script {
    pub name: String,

    pub icon_name: Option<String>,

    pub backup_script: String,

    pub backup_path: Option<PathBuf>,

    #[serde(default, with = "humantime_serde")]
    pub interval: Duration,

    #[serde(default, with = "humantime_serde")]
    pub reminder: Option<Duration>,

    #[serde(default)]
    pub post_backup_actions: Vec<PostScriptAction>,

    pub last_backup: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Settings {
    pub icon_name: String,

    pub title: String,

    pub scripts: Vec<Script>,

    pub autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            icon_name: "backup".to_string(),
            title: "Backup".to_string(),
            scripts: Vec::new(),
            autostart: false,
        }
    }
}

impl Settings {
    pub fn load() -> anyhow::Result<Settings> {
        let settings_file_path = settings_file_path()?;

        if !settings_file_path.exists() {
            let settings = Settings::default();
            settings.save()?;
        }

        let file = File::open(settings_file_path)?;
        let settings = serde_yaml_ng::from_reader::<_, Settings>(&file)?;

        let script_names = settings
            .scripts
            .iter()
            .map(|script| &script.name)
            .collect::<HashSet<_>>();
        ensure!(
            script_names.len() == settings.scripts.len(),
            "script names must be unique"
        );

        log::trace!("settings loaded: {settings:#?}");

        Ok(settings)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let settings_file_path = settings_file_path()?;

        let mut file = File::create(settings_file_path)?;
        file.write_all(b"# see https://github.com/dfaust/backup-monitor/blob/master/README.md for instructions\n")?;
        serde_yaml_ng::to_writer(&file, self)?;

        log::trace!("settings saved");

        Ok(())
    }
}

pub fn settings_file_path() -> anyhow::Result<PathBuf> {
    let config_dir = dirs::config_dir().context("config dir not found")?;
    Ok(config_dir.join("backup-monitor.yaml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn deserialize_empty() {
        let settings = serde_yaml_ng::from_str::<Settings>("").unwrap();

        insta::assert_yaml_snapshot!(settings);
    }

    #[test]
    fn deserialize_minimal() {
        let yaml = indoc! {"
            scripts:
            - name: Backup
              backup-script: |
                #!/usr/bin/env bash
                set -o errexit
                /usr/bin/backup.sh
              interval: 1day
        "};
        let settings = serde_yaml_ng::from_str::<Settings>(yaml).unwrap();

        insta::assert_yaml_snapshot!(settings);
    }

    #[test]
    fn deserialize_full() {
        let yaml = indoc! {"
            icon-name: backup
            title: Backup
            scripts:
            - name: Backup
              icon-name: null
              backup-script: |
                #!/usr/bin/env bash
                set -o errexit
                /usr/bin/backup.sh
              backup-path: /mnt/backup
              interval: 1day
              reminder: 7days
              post-backup-actions:
                - label: Unmount backup HDD
                  script: |
                    #!/usr/bin/env bash
                    set -o errexit
                    umount /mnt/backup
              last-backup: 2024-10-24T20:18:00.857399073Z
            autostart: true
        "};
        let settings = serde_yaml_ng::from_str::<Settings>(yaml).unwrap();

        insta::assert_yaml_snapshot!(settings);
    }
}
