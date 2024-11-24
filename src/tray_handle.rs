use serde::{Deserialize, Deserializer};

use crate::tray::Tray;

#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
pub struct TrayData {
    #[serde(deserialize_with = "deserialize_status")]
    pub status: Option<ksni::Status>,
    pub tooltip: Option<String>,
    pub scripts: Option<Vec<(String, Option<String>)>>,
}

pub fn deserialize_status<'de, D>(deserializer: D) -> Result<Option<ksni::Status>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        Some(s) => match s.to_lowercase().as_str() {
            "passive" => Ok(Some(ksni::Status::Passive)),
            "active" => Ok(Some(ksni::Status::Active)),
            "needsattention" => Ok(Some(ksni::Status::NeedsAttention)),
            _ => Err(serde::de::Error::custom(format!("Invalid status: {s}"))),
        },
        None => Ok(None),
    }
}

pub trait TrayHandle<T: ksni::Tray> {
    fn update(&self, data: TrayData);
}

impl TrayHandle<Tray> for ksni::Handle<Tray> {
    fn update(&self, data: TrayData) {
        self.update(|tray| {
            if let Some(status) = data.status {
                tray.set_status(status);
            }
            if let Some(tooltip) = data.tooltip {
                tray.set_tooltip(tooltip);
            }
            if let Some(scripts) = data.scripts {
                tray.set_scripts(scripts);
            }
        });
    }
}
