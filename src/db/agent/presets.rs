//! Which model provider the Agents panel talks to.
//!
//! A provider is either one of the built-in [`PRESETS`] — a hosted API whose
//! address is known, or a local server on its usual port — or a
//! [`CustomProvider`] the user saved: any OpenAI-compatible endpoint, as many
//! as they like. Both are named by a [`ProviderKey`], the string the settings
//! file stores and the keychain files the provider's API key under, so a
//! renamed custom provider keeps its key.
//!
//! Nothing here talks to a network or a window; [`Resolved`] is what the panel
//! hands to [`super::provider::connect`] once it has read the key.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The wire format a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `POST {base}/chat/completions`, bearer key: OpenAI and everything
    /// that copies its shape.
    OpenAiChat,
    /// `POST {base}/messages`, `x-api-key`: Anthropic's Messages API.
    Anthropic,
}

/// A built-in provider.
#[derive(Debug, PartialEq, Eq)]
pub struct Preset {
    /// What the settings file and the keychain call it. Never changes once
    /// shipped, or a saved choice and its key would be lost.
    pub key: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    /// The model filled in when the provider is first picked. The user can
    /// change it; empty for a local server, whose models are the user's own.
    pub model: &'static str,
    pub protocol: Protocol,
    /// A server on this machine: its address can be changed (another port,
    /// another host on the network) and it needs no key.
    pub local: bool,
}

/// The built-in providers, in the order the dropdown lists them.
pub const PRESETS: &[Preset] = &[
    Preset {
        key: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        model: "gpt-5",
        protocol: Protocol::OpenAiChat,
        local: false,
    },
    Preset {
        key: "anthropic",
        name: "Anthropic",
        base_url: "https://api.anthropic.com/v1",
        model: "claude-opus-5-5",
        protocol: Protocol::Anthropic,
        local: false,
    },
    Preset {
        key: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        model: "deepseek-chat",
        protocol: Protocol::OpenAiChat,
        local: false,
    },
    Preset {
        key: "gemini",
        name: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        model: "gemini-2.5-flash",
        protocol: Protocol::OpenAiChat,
        local: false,
    },
    Preset {
        key: "mistral",
        name: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        model: "mistral-large-latest",
        protocol: Protocol::OpenAiChat,
        local: false,
    },
    Preset {
        key: "xai",
        name: "xAI",
        base_url: "https://api.x.ai/v1",
        model: "grok-4",
        protocol: Protocol::OpenAiChat,
        local: false,
    },
    Preset {
        key: "groq",
        name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        model: "llama-3.3-70b-versatile",
        protocol: Protocol::OpenAiChat,
        local: false,
    },
    Preset {
        key: "openrouter",
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        model: "openai/gpt-5",
        protocol: Protocol::OpenAiChat,
        local: false,
    },
    Preset {
        key: "ollama",
        name: "Ollama (local)",
        base_url: "http://localhost:11434/v1",
        model: "",
        protocol: Protocol::OpenAiChat,
        local: true,
    },
    Preset {
        key: "lmstudio",
        name: "LM Studio (local)",
        base_url: "http://localhost:1234/v1",
        model: "",
        protocol: Protocol::OpenAiChat,
        local: true,
    },
];

/// What the user changed about a built-in provider. Empty fields mean the
/// preset's own value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetChoice {
    pub model: String,
    /// Only a local preset's address can be changed; a hosted one's is fixed.
    pub base_url: String,
}

/// An OpenAI-compatible endpoint the user saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomProvider {
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    /// Up to but not including `/chat/completions`.
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// The provider was made from the single endpoint older versions kept,
    /// whose key is still under the old keychain account until a new key is
    /// saved for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy_key: bool,
}

impl CustomProvider {
    pub fn new(name: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            base_url: String::new(),
            model: String::new(),
            legacy_key: false,
        }
    }

    /// The name shown for it, never blank.
    pub fn label(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            "Unnamed provider".into()
        } else {
            name.to_string()
        }
    }
}

/// Which provider, as the settings file stores it: a preset's key, or
/// `custom:<uuid>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKey {
    Preset(&'static Preset),
    Custom(Uuid),
}

const CUSTOM_PREFIX: &str = "custom:";

impl ProviderKey {
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(id) = text.strip_prefix(CUSTOM_PREFIX) {
            return Uuid::parse_str(id).ok().map(ProviderKey::Custom);
        }
        preset(text).map(ProviderKey::Preset)
    }

    /// The stored form, also the provider's keychain account suffix.
    pub fn as_string(&self) -> String {
        match self {
            ProviderKey::Preset(preset) => preset.key.to_string(),
            ProviderKey::Custom(id) => format!("{CUSTOM_PREFIX}{id}"),
        }
    }
}

pub fn preset(key: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.key == key)
}

/// The provider the settings point at, with everything a request needs but
/// the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub key: ProviderKey,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub protocol: Protocol,
    /// Whether a request without a key is pointless (a hosted API). A custom
    /// endpoint may or may not want one, so it is never required.
    pub key_required: bool,
    /// Read the key from the keychain account older versions used.
    pub legacy_key: bool,
}

/// The agent settings, as `settings::Settings` keeps them.
#[derive(Debug, Clone, Copy)]
pub struct Choices<'a> {
    pub selected: &'a str,
    pub presets: &'a std::collections::BTreeMap<String, PresetChoice>,
    pub custom: &'a [CustomProvider],
}

impl Choices<'_> {
    /// The selected provider, or `None` when nothing (or something since
    /// deleted) is selected.
    pub fn resolve(&self) -> Option<Resolved> {
        match ProviderKey::parse(self.selected)? {
            key @ ProviderKey::Preset(preset) => {
                let choice = self.presets.get(preset.key);
                let model = choice
                    .map(|choice| choice.model.trim())
                    .filter(|model| !model.is_empty())
                    .unwrap_or(preset.model);
                let base_url = choice
                    .filter(|_| preset.local)
                    .map(|choice| choice.base_url.trim())
                    .filter(|url| !url.is_empty())
                    .unwrap_or(preset.base_url);
                Some(Resolved {
                    key,
                    name: preset.name.to_string(),
                    base_url: base_url.to_string(),
                    model: model.to_string(),
                    protocol: preset.protocol,
                    key_required: !preset.local,
                    legacy_key: false,
                })
            }
            key @ ProviderKey::Custom(id) => {
                let custom = self.custom.iter().find(|custom| custom.id == id)?;
                Some(Resolved {
                    key,
                    name: custom.label(),
                    base_url: custom.base_url.trim().to_string(),
                    model: custom.model.trim().to_string(),
                    protocol: Protocol::OpenAiChat,
                    key_required: false,
                    legacy_key: custom.legacy_key,
                })
            }
        }
    }

    /// Every provider the dropdown offers, as (stored key, label): the
    /// presets, then the saved custom providers in the order they were added.
    pub fn options(&self) -> Vec<(String, String)> {
        PRESETS
            .iter()
            .map(|preset| (preset.key.to_string(), preset.name.to_string()))
            .chain(self.custom.iter().map(|custom| {
                (
                    ProviderKey::Custom(custom.id).as_string(),
                    format!("{} (custom)", custom.label()),
                )
            }))
            .collect()
    }
}

/// Why a turn cannot start with the provider as configured, in words for
/// the panel; `None` when it can.
pub fn missing_setting(resolved: Option<&Resolved>) -> Option<String> {
    let Some(resolved) = resolved else {
        return Some("Choose a model provider in Settings › Agents first.".into());
    };
    if resolved.base_url.is_empty() {
        return Some(format!(
            "Set {}'s base URL in Settings › Agents first.",
            resolved.name
        ));
    }
    if resolved.model.is_empty() {
        return Some(format!(
            "Set the model {} should use in Settings › Agents first.",
            resolved.name
        ));
    }
    None
}

/// A name for an endpoint saved without one: its host and port, as the URL
/// gives them (`localhost:11434` for `http://localhost:11434/v1`).
pub fn name_for_url(base_url: &str) -> String {
    let rest = base_url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Never show credentials a URL might carry.
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    if host.is_empty() {
        "Custom endpoint".into()
    } else {
        host.to_string()
    }
}

/// A name for a new custom provider that no saved one already has.
pub fn next_custom_name(custom: &[CustomProvider]) -> String {
    let taken = |name: &str| custom.iter().any(|custom| custom.name.trim() == name);
    let mut number = custom.len() + 1;
    loop {
        let name = format!("Custom provider {number}");
        if !taken(&name) {
            return name;
        }
        number += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn choices<'a>(
        selected: &'a str,
        presets: &'a BTreeMap<String, PresetChoice>,
        custom: &'a [CustomProvider],
    ) -> Choices<'a> {
        Choices {
            selected,
            presets,
            custom,
        }
    }

    #[test]
    fn preset_keys_are_unique_and_parse_back() {
        for (index, preset) in PRESETS.iter().enumerate() {
            assert!(
                PRESETS[index + 1..]
                    .iter()
                    .all(|other| other.key != preset.key),
                "{} is listed twice",
                preset.key
            );
            assert_eq!(
                ProviderKey::parse(preset.key),
                Some(ProviderKey::Preset(preset))
            );
            assert!(!preset.key.starts_with(CUSTOM_PREFIX));
        }
        let id = Uuid::new_v4();
        let key = ProviderKey::Custom(id);
        assert_eq!(ProviderKey::parse(&key.as_string()), Some(key));
        assert_eq!(ProviderKey::parse("custom:not-a-uuid"), None);
        assert_eq!(ProviderKey::parse("nobody"), None);
    }

    #[test]
    fn a_hosted_preset_keeps_its_address_and_falls_back_to_its_model() {
        let mut presets = BTreeMap::new();
        presets.insert(
            "anthropic".to_string(),
            PresetChoice {
                model: "  ".into(),
                base_url: "http://elsewhere".into(),
            },
        );
        let resolved = choices("anthropic", &presets, &[]).resolve().unwrap();
        assert_eq!(resolved.base_url, "https://api.anthropic.com/v1");
        assert_eq!(resolved.model, "claude-opus-5-5");
        assert_eq!(resolved.protocol, Protocol::Anthropic);
        assert!(resolved.key_required);
    }

    #[test]
    fn a_local_preset_can_move_and_needs_a_model_of_the_users_own() {
        let mut presets = BTreeMap::new();
        let resolved = choices("ollama", &presets, &[]).resolve().unwrap();
        assert!(!resolved.key_required);
        assert!(missing_setting(Some(&resolved)).unwrap().contains("model"));

        presets.insert(
            "ollama".to_string(),
            PresetChoice {
                model: "llama3.2".into(),
                base_url: "http://gpu-box:11434/v1".into(),
            },
        );
        let resolved = choices("ollama", &presets, &[]).resolve().unwrap();
        assert_eq!(resolved.base_url, "http://gpu-box:11434/v1");
        assert_eq!(missing_setting(Some(&resolved)), None);
    }

    #[test]
    fn custom_providers_resolve_by_id_and_a_deleted_one_resolves_to_none() {
        let mut first = CustomProvider::new("Gateway".into());
        first.base_url = " https://gateway.example/v1 ".into();
        first.model = "big".into();
        let second = CustomProvider::new(String::new());
        let custom = vec![first.clone(), second.clone()];
        let presets = BTreeMap::new();

        let selected = ProviderKey::Custom(first.id).as_string();
        let resolved = choices(&selected, &presets, &custom).resolve().unwrap();
        assert_eq!(resolved.name, "Gateway");
        assert_eq!(resolved.base_url, "https://gateway.example/v1");
        assert_eq!(resolved.protocol, Protocol::OpenAiChat);
        assert!(!resolved.key_required);

        let selected = ProviderKey::Custom(second.id).as_string();
        let resolved = choices(&selected, &presets, &custom).resolve().unwrap();
        assert_eq!(resolved.name, "Unnamed provider");
        assert!(
            missing_setting(Some(&resolved))
                .unwrap()
                .contains("base URL")
        );

        let gone = ProviderKey::Custom(Uuid::new_v4()).as_string();
        assert_eq!(choices(&gone, &presets, &custom).resolve(), None);
        assert!(missing_setting(None).is_some());

        let options = choices("", &presets, &custom).options();
        assert_eq!(options.len(), PRESETS.len() + 2);
        assert_eq!(options[PRESETS.len()].1, "Gateway (custom)");
    }

    #[test]
    fn an_endpoint_is_named_for_its_host() {
        assert_eq!(name_for_url("http://localhost:11434/v1"), "localhost:11434");
        assert_eq!(
            name_for_url("https://user:secret@gateway.example/v1?x=1"),
            "gateway.example"
        );
        assert_eq!(name_for_url(""), "Custom endpoint");
    }

    #[test]
    fn new_custom_names_do_not_repeat() {
        let mut custom = vec![CustomProvider::new("Custom provider 2".into())];
        assert_eq!(next_custom_name(&custom), "Custom provider 3");
        custom.push(CustomProvider::new("Custom provider 3".into()));
        assert_eq!(next_custom_name(&custom), "Custom provider 4");
        assert_eq!(next_custom_name(&[]), "Custom provider 1");
    }
}
