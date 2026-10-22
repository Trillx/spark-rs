//! The provider choice, saved as `ai.json` in the app's config directory.
//! The API key is not part of it; see `keys.rs`.

use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::{Deserialize, Serialize};
use spark_ai::{Effort, OPENAI_BASE_URL, ProviderConfig};

const FILE_NAME: &str = "ai.json";

/// The three choices in Settings → AI. Each keeps its own key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Anthropic,
    OpenAi,
    /// Any OpenAI-compatible server: OpenRouter, Ollama, LM Studio, vLLM, …
    Custom,
}

impl Preset {
    pub fn id(self) -> &'static str {
        match self {
            Preset::Anthropic => "anthropic",
            Preset::OpenAi => "openai",
            Preset::Custom => "custom",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Preset::Anthropic => "Anthropic",
            Preset::OpenAi => "OpenAI",
            Preset::Custom => "custom server",
        }
    }

    /// Hosted APIs need a key; local servers often don't.
    pub fn needs_key(self) -> bool {
        self != Preset::Custom
    }

    pub fn defaults(self) -> ProviderConfig {
        match self {
            Preset::Anthropic => ProviderConfig::anthropic(),
            Preset::OpenAi => ProviderConfig::openai_compatible(OPENAI_BASE_URL, ""),
            Preset::Custom => ProviderConfig::openai_compatible("", ""),
        }
    }

    /// Fits a config from the UI to this preset: the preset's API shape, its
    /// own base URL when the field is left empty, and effort only for
    /// Anthropic. Empty fields are allowed so a half-filled form can be saved;
    /// [`AiSettings::missing`] reports them when AI is used.
    pub fn normalize(self, config: ProviderConfig) -> Result<ProviderConfig, String> {
        let defaults = self.defaults();
        let base_url = match config.base_url.trim() {
            "" => defaults.base_url,
            url if url.starts_with("http://") || url.starts_with("https://") => url.to_owned(),
            _ => {
                return Err(
                    "The base URL must start with http:// or https://, e.g. http://localhost:11434/v1."
                        .into(),
                );
            }
        };
        let effort = match self {
            Preset::Anthropic => config.effort.or(Some(Effort::Medium)),
            Preset::OpenAi | Preset::Custom => None,
        };
        Ok(ProviderConfig {
            kind: defaults.kind,
            base_url,
            model: config.model.trim().to_owned(),
            effort,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    pub preset: Preset,
    pub config: ProviderConfig,
}

impl AiSettings {
    fn path(dir: &Path) -> PathBuf {
        dir.join(FILE_NAME)
    }

    /// `None` until the user saves AI settings for the first time.
    pub fn load(dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::path(dir);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("Couldn't read {}: {e}", path.display())),
        };
        serde_json::from_str(&text).map(Some).map_err(|e| {
            format!(
                "{} is damaged ({e}). Saving the AI settings again replaces it.",
                path.display()
            )
        })
    }

    /// Writes a temporary file and renames it over `ai.json`, so a crash
    /// never leaves half a file behind.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let path = Self::path(dir);
        let temp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).expect("settings serialize");
        fs::create_dir_all(dir)
            .and_then(|()| fs::write(&temp, json))
            .and_then(|()| fs::rename(&temp, &path))
            .map_err(|e| format!("Couldn't save {}: {e}", path.display()))
    }

    /// What has to be filled in before AI can run, in words for the UI.
    pub fn missing(&self, has_key: bool, need_model: bool) -> Option<String> {
        if self.preset.needs_key() && !has_key {
            Some(format!(
                "Add your {} API key in Settings → AI.",
                self.preset.label()
            ))
        } else if self.config.base_url.is_empty() {
            Some("Enter your server's base URL in Settings → AI.".into())
        } else if need_model && self.config.model.is_empty() {
            Some("Choose a model in Settings → AI.".into())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use spark_ai::{ANTHROPIC_BASE_URL, ANTHROPIC_DEFAULT_MODEL, ProviderKind};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spark-rs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn serializes_preset_and_config_without_a_key() {
        let settings = AiSettings {
            preset: Preset::Anthropic,
            config: ProviderConfig::anthropic(),
        };
        assert_eq!(
            serde_json::to_value(&settings).unwrap(),
            json!({
                "preset": "anthropic",
                "config": {
                    "kind": "anthropic",
                    "baseUrl": ANTHROPIC_BASE_URL,
                    "model": ANTHROPIC_DEFAULT_MODEL,
                    "effort": "medium",
                },
            })
        );
        let custom: AiSettings = serde_json::from_value(json!({
            "preset": "custom",
            "config": { "kind": "open-ai-compatible", "baseUrl": "http://localhost:11434/v1", "model": "llama3.3" },
        }))
        .unwrap();
        assert_eq!(custom.preset, Preset::Custom);
        assert_eq!(custom.config.effort, None);
        assert_eq!(serde_json::to_value(Preset::OpenAi).unwrap(), "openai");
    }

    #[test]
    fn a_missing_file_means_not_configured() {
        let dir = temp_dir("settings-missing");
        assert_eq!(AiSettings::load(&dir), Ok(None));
    }

    #[test]
    fn saves_and_loads_round_trip() {
        let dir = temp_dir("settings-roundtrip");
        let settings = AiSettings {
            preset: Preset::Custom,
            config: ProviderConfig::openai_compatible("http://localhost:1234/v1", "qwen3"),
        };
        settings.save(&dir).unwrap();
        assert_eq!(AiSettings::load(&dir), Ok(Some(settings.clone())));
        assert!(!dir.join("ai.json.tmp").exists());

        fs::write(dir.join(FILE_NAME), "{ not json").unwrap();
        assert!(AiSettings::load(&dir).unwrap_err().contains("damaged"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn normalize_fits_the_config_to_the_preset() {
        let from_ui = ProviderConfig {
            kind: ProviderKind::Anthropic,
            base_url: "  ".into(),
            model: " gpt-5.5 ".into(),
            effort: Some(Effort::High),
        };
        let openai = Preset::OpenAi.normalize(from_ui.clone()).unwrap();
        assert_eq!(openai.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(openai.base_url, OPENAI_BASE_URL);
        assert_eq!(openai.model, "gpt-5.5");
        assert_eq!(openai.effort, None);

        let anthropic = Preset::Anthropic
            .normalize(ProviderConfig {
                effort: None,
                ..from_ui.clone()
            })
            .unwrap();
        assert_eq!(anthropic.base_url, ANTHROPIC_BASE_URL);
        assert_eq!(anthropic.effort, Some(Effort::Medium));

        let custom = Preset::Custom.normalize(from_ui.clone()).unwrap();
        assert_eq!(custom.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(custom.base_url, "");
        assert!(
            Preset::Custom
                .normalize(ProviderConfig {
                    base_url: "localhost:11434".into(),
                    ..from_ui
                })
                .is_err()
        );
    }

    #[test]
    fn missing_names_the_first_thing_to_fill_in() {
        let mut settings = AiSettings {
            preset: Preset::OpenAi,
            config: Preset::OpenAi.defaults(),
        };
        assert_eq!(
            settings.missing(false, true).as_deref(),
            Some("Add your OpenAI API key in Settings → AI.")
        );
        assert_eq!(
            settings.missing(true, true).as_deref(),
            Some("Choose a model in Settings → AI.")
        );
        assert_eq!(settings.missing(true, false), None);

        settings.preset = Preset::Custom;
        settings.config = Preset::Custom.defaults();
        assert!(settings.missing(false, false).unwrap().contains("base URL"));
        settings.config.base_url = "http://localhost:11434/v1".into();
        settings.config.model = "llama3.3".into();
        assert_eq!(settings.missing(false, true), None);
    }
}
