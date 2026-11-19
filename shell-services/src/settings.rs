//! Configurações persistidas do shell.
//!
//! Fonte única em `~/.config/spatial-deck/settings.json`. O frontend guarda
//! uma cópia em `localStorage` apenas como cache/fallback; ao conectar, pede
//! `SettingsRequest` e reenvia o que só existia no navegador (migração).

use std::{fs, path::PathBuf};

use common::IpcMessage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub theme: Option<String>,
    /// Layout da barra como JSON opaco (quem entende é o frontend).
    #[serde(default)]
    pub bar: Option<Value>,
    #[serde(default)]
    pub night_light: bool,
    /// Bloqueia a tela após N minutos sem input (0 = desligado).
    #[serde(default)]
    pub lock_after_minutes: u32,
}

impl Settings {
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(error) = fs::write(&path, text) {
                    tracing::warn!(%error, "falha ao gravar settings.json");
                }
            }
            Err(error) => tracing::warn!(%error, "falha ao serializar settings"),
        }
    }

    pub fn state(&self) -> IpcMessage {
        IpcMessage::SettingsState {
            theme: self.theme.clone(),
            bar: self.bar.clone(),
            night_light: self.night_light,
            lock_after_minutes: self.lock_after_minutes,
        }
    }

    /// Aplica uma configuração. Devolve `false` para chave/valor desconhecido.
    pub fn set(&mut self, key: &str, value: &Value) -> bool {
        match key {
            "theme" => {
                if value.is_null() {
                    self.theme = None;
                    true
                } else {
                    match value.as_str() {
                        Some(theme) => {
                            self.theme = Some(theme.to_owned());
                            true
                        }
                        None => false,
                    }
                }
            }
            "bar" => {
                if value.is_null() {
                    self.bar = None;
                    true
                } else if value.is_object() {
                    self.bar = Some(value.clone());
                    true
                } else {
                    false
                }
            }
            "night-light" => match value.as_bool() {
                Some(enabled) => {
                    self.night_light = enabled;
                    true
                }
                None => false,
            },
            "lock-after" => match value.as_u64() {
                Some(minutes) if minutes <= 1440 => {
                    self.lock_after_minutes = minutes as u32;
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("spatial-deck").join("settings.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_known_keys() {
        let mut settings = Settings::default();
        assert!(settings.set("theme", &serde_json::json!("nord")));
        assert_eq!(settings.theme.as_deref(), Some("nord"));

        assert!(settings.set("theme", &Value::Null));
        assert_eq!(settings.theme, None);

        assert!(settings.set("bar", &serde_json::json!({ "hour24": false })));
        assert_eq!(
            settings.bar,
            Some(serde_json::json!({ "hour24": false }))
        );

        assert!(settings.set("night-light", &serde_json::json!(true)));
        assert!(settings.night_light);

        assert!(settings.set("lock-after", &serde_json::json!(5)));
        assert_eq!(settings.lock_after_minutes, 5);
        assert!(settings.set("lock-after", &serde_json::json!(0)));
        assert_eq!(settings.lock_after_minutes, 0);

        assert!(!settings.set("theme", &serde_json::json!(3)));
        assert!(!settings.set("bar", &serde_json::json!("não é objeto")));
        assert!(!settings.set("night-light", &serde_json::json!("sim")));
        assert!(!settings.set("lock-after", &serde_json::json!(-1)));
        assert!(!settings.set("lock-after", &serde_json::json!(1441)));
        assert!(!settings.set("inexistente", &serde_json::json!(1)));
    }

    #[test]
    fn state_round_trips_through_json() {
        let settings = Settings {
            theme: Some("minimalist".into()),
            bar: Some(serde_json::json!({ "hour24": true, "modules": [] })),
            night_light: false,
            lock_after_minutes: 10,
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, settings);

        let state = settings.state();
        let json = state.to_json().unwrap();
        assert!(json.contains("\"type\":\"settings_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), state);
    }

    #[test]
    fn missing_file_defaults() {
        assert_eq!(Settings::default().state(), Settings::default().state());
    }
}
