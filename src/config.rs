// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! Configuration compatible with the original WPF app's `config.json`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const APP_DATA_DIR_NAME: &str = "WSL USB Manager";
pub const CONFIG_FILE_NAME: &str = "config.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct UsbDevice {
    #[serde(default)]
    pub instance_id: String,
    #[serde(default)]
    pub hardware_id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub is_forced: bool,
    #[serde(default)]
    pub bus_id: String,
    #[serde(default)]
    pub persisted_guid: String,
    #[serde(default)]
    pub stub_instance_id: String,
    #[serde(default)]
    pub client_ip_address: String,
    #[serde(default)]
    pub is_bound: bool,
    #[serde(default)]
    pub is_connected: bool,
    #[serde(default)]
    pub is_attached: bool,
    #[serde(default)]
    pub name: String,
}

impl UsbDevice {
    pub fn short_name(&self) -> String {
        if self.description.trim().is_empty() {
            if self.hardware_id.trim().is_empty() {
                self.bus_id.clone()
            } else {
                self.hardware_id.clone()
            }
        } else {
            self.description
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .to_owned()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct AppConfig {
    #[serde(default)]
    pub dark_mode: bool,
    #[serde(default)]
    pub lang: String,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default)]
    pub use_bus_id: bool,
    #[serde(default)]
    pub specify_net_card: bool,
    #[serde(default)]
    pub forward_net_card: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            dark_mode: false,
            lang: String::new(),
            close_to_tray: true,
            use_bus_id: false,
            specify_net_card: false,
            forward_net_card: String::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemConfig {
    #[serde(default)]
    pub app_config: AppConfig,
    #[serde(default)]
    pub auto_attach_device_list: Vec<UsbDevice>,
    #[serde(default)]
    pub filtered_device_list: Vec<UsbDevice>,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            app_config: AppConfig::default(),
            auto_attach_device_list: Vec::new(),
            filtered_device_list: Vec::new(),
        }
    }
}

impl SystemConfig {
    pub fn is_in_auto_list(&self, hardware_id: &str) -> bool {
        !hardware_id.is_empty()
            && self
                .auto_attach_device_list
                .iter()
                .any(|d| d.hardware_id.eq_ignore_ascii_case(hardware_id))
    }

    pub fn is_in_filter_list(&self, hardware_id: &str) -> bool {
        !hardware_id.is_empty()
            && self
                .filtered_device_list
                .iter()
                .any(|d| d.hardware_id.eq_ignore_ascii_case(hardware_id))
    }

    pub fn add_to_auto_list(&mut self, dev: &UsbDevice) {
        if !self.is_in_auto_list(&dev.hardware_id) {
            self.auto_attach_device_list.push(dev.clone());
        }
    }

    pub fn remove_from_auto_list(&mut self, hardware_id: &str) {
        self.auto_attach_device_list
            .retain(|d| !d.hardware_id.eq_ignore_ascii_case(hardware_id));
    }

    pub fn add_to_filter_list(&mut self, dev: &UsbDevice) {
        if !self.is_in_filter_list(&dev.hardware_id) {
            self.filtered_device_list.push(dev.clone());
        }
    }

    pub fn remove_from_filter_list(&mut self, hardware_id: &str) {
        self.filtered_device_list
            .retain(|d| !d.hardware_id.eq_ignore_ascii_case(hardware_id));
    }

    pub fn reset(&mut self) {
        *self = SystemConfig::default();
    }
}

/// `%APPDATA%\WSL USB Manager`
pub fn app_data_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        Path::new(&appdata).join(APP_DATA_DIR_NAME)
    } else if let Ok(profile) = std::env::var("USERPROFILE") {
        Path::new(&profile)
            .join("AppData")
            .join("Roaming")
            .join(APP_DATA_DIR_NAME)
    } else {
        PathBuf::from(APP_DATA_DIR_NAME)
    }
}

pub fn config_path() -> PathBuf {
    app_data_dir().join(CONFIG_FILE_NAME)
}

pub fn load() -> SystemConfig {
    let dir = app_data_dir();
    let _ = fs::create_dir_all(&dir);
    let path = config_path();
    if let Ok(text) = fs::read_to_string(&path) {
        match serde_json::from_str::<SystemConfig>(&text) {
            Ok(cfg) => return cfg,
            Err(e) => crate::log::error(&format!("Failed to parse config.json: {e}")),
        }
    }
    SystemConfig::default()
}

pub fn save(cfg: &SystemConfig) {
    let dir = app_data_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        crate::log::error(&format!("Failed to create config dir {dir:?}: {e}"));
        return;
    }
    match serde_json::to_string_pretty(cfg) {
        Ok(json) => {
            if let Err(e) = fs::write(config_path(), json) {
                crate::log::error(&format!("Failed to save config: {e}"));
            }
        }
        Err(e) => crate::log::error(&format!("Failed to serialize config: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_matches_original_pascal_case_shape() {
        let mut cfg = SystemConfig::default();
        cfg.app_config.lang = "zh".to_owned();
        cfg.app_config.forward_net_card = "以太网".to_owned();
        cfg.auto_attach_device_list.push(UsbDevice {
            hardware_id: "USB\\VID_1234&PID_5678".to_owned(),
            is_bound: true,
            ..Default::default()
        });
        let json = serde_json::to_value(&cfg).unwrap();
        assert!(json["AppConfig"]["ForwardNetCard"].is_string());
        assert!(json["AutoAttachDeviceList"][0]["HardwareId"].is_string());
        assert_eq!(json["AppConfig"]["Lang"], "zh");

        // Round trip.
        let back: SystemConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.app_config.lang, "zh");
        assert_eq!(
            back.auto_attach_device_list[0].hardware_id,
            "USB\\VID_1234&PID_5678"
        );
    }
}
