use std::{process::Command, sync::mpsc::Sender};

use crate::{
    settings::{settings_file_path, Settings},
    Event,
};

pub struct Tray {
    icon_name: String,
    title: String,
    status: ksni::Status,
    tooltip: String,
    scripts: Vec<(String, Option<String>)>,
    tx: Sender<Event>,
}

impl Tray {
    pub fn new(settings: &Settings, tx: Sender<Event>) -> Tray {
        Tray {
            icon_name: settings.icon_name.clone(),
            title: settings.title.clone(),
            status: ksni::Status::Passive,
            tooltip: String::new(),
            scripts: Vec::new(),
            tx,
        }
    }

    pub fn set_status(&mut self, status: ksni::Status) {
        self.status = status;
    }

    pub fn set_tooltip(&mut self, tooltip: String) {
        self.tooltip = tooltip;
    }

    pub fn set_scripts(&mut self, scripts: Vec<(String, Option<String>)>) {
        self.scripts = scripts;
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }

    fn status(&self) -> ksni::Status {
        self.status
    }

    fn icon_name(&self) -> String {
        self.icon_name.clone()
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: "backup".to_string(),
            title: "Backup Monitor".to_string(),
            description: self.tooltip.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        let mut items = Vec::new();

        for (script_name, icon_name) in &self.scripts {
            let tx = self.tx.clone();
            let name = script_name.clone();

            items.push(
                StandardItem {
                    label: format!("Run {script_name} now"),
                    icon_name: icon_name.as_deref().unwrap_or("system-run").to_string(),
                    activate: Box::new(move |_| {
                        let _ = tx.send(Event::ManualRun(name.clone()));
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: "Settings".to_string(),
                icon_name: "settings-configure".to_string(),
                activate: Box::new(|_| {
                    let settings_file_path = settings_file_path()
                        .unwrap()
                        .to_str()
                        .map(str::to_string)
                        .unwrap();
                    let _ = Command::new("xdg-open").arg(settings_file_path).spawn();
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(
            StandardItem {
                label: "Exit".to_string(),
                icon_name: "application-exit".to_string(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );

        items
    }
}
