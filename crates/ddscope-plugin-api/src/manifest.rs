//! External `plugin.json` descriptor schemas shared between the Rust engine and UI tooling.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Schema version tag required in external protocol plugin `plugin.json` descriptors.
pub const PROTOCOL_PLUGIN_SCHEMA_V1: &str = "ddscope.protocol-plugin.v1";

/// Schema version tag required in external widget plugin `plugin.json` descriptors.
pub const WIDGET_PLUGIN_SCHEMA_V1: &str = "ddscope.widget-plugin.v1";

/// Validates that a plugin ID is safe for registry keys, widget type tags, and directory names.
pub fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// Validates that a bundle asset path (e.g., `index.js`, `style.css`) stays relative and traversal-free.
pub fn is_safe_relative_asset_path(rel_path: &str) -> bool {
    let trimmed = rel_path.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains(':')
    {
        return false;
    }
    let parts = trimmed.split(['/', '\\']).collect::<Vec<_>>();
    !parts.iter().any(|seg| *seg == ".." || seg.is_empty())
}

/// Human-readable text either as a plain string or `{ "en": "...", "zh-CN": "..." }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocalizedText {
    Plain(String),
    Map {
        en: String,
        #[serde(rename = "zh-CN", default)]
        zh_cn: Option<String>,
    },
}

impl Default for LocalizedText {
    fn default() -> Self {
        Self::Plain(String::new())
    }
}

impl LocalizedText {
    /// Returns the English variant (or plain text).
    pub fn en(&self) -> &str {
        match self {
            Self::Plain(s) => s.as_str(),
            Self::Map { en, .. } => en.as_str(),
        }
    }

    /// Returns the Simplified Chinese variant (falling back to English/plain text).
    pub fn zh_cn(&self) -> &str {
        match self {
            Self::Plain(s) => s.as_str(),
            Self::Map { en, zh_cn } => zh_cn.as_deref().unwrap_or(en.as_str()),
        }
    }

    /// Resolves the best variant matching `locale`.
    pub fn resolve(&self, locale: &str) -> &str {
        if locale.eq_ignore_ascii_case("zh-cn") || locale.eq_ignore_ascii_case("zh") {
            self.zh_cn()
        } else {
            self.en()
        }
    }
}

/// Supported parameter editor control types declared inside `plugin.json`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginParamType {
    Number,
    #[default]
    String,
    Boolean,
    Select,
}

impl PluginParamType {
    /// Returns the lowercase wire identifier used by frontend form generators.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::String => "string",
            Self::Boolean => "boolean",
            Self::Select => "select",
        }
    }
}

/// Option entry for a [`PluginParamType::Select`] parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSelectOption {
    pub value: String,
    pub label: LocalizedText,
}

/// Declarative parameter schema displayed in SourceWizard / Widget settings and merged with JSON config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginParamSpec {
    pub key: String,
    pub label: LocalizedText,
    #[serde(rename = "type", default)]
    pub param_type: PluginParamType,
    #[serde(rename = "default", default)]
    pub default_value: Value,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub options: Vec<PluginSelectOption>,
    #[serde(default)]
    pub description: Option<LocalizedText>,
}

/// Static channel hint declared in `plugin.json` for UI previews (mock mode & sync selector).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginChannelPreview {
    pub name: String,
    pub scalar_type: String,
    #[serde(default)]
    pub unit: Option<String>,
}

fn default_little_str() -> String {
    "little".to_string()
}

fn default_none_str() -> String {
    "none".to_string()
}

fn default_payload_only_str() -> String {
    "payload_only".to_string()
}

fn default_float_str() -> String {
    "float".to_string()
}

/// Field description for declarative [`DeclarativeProtocolSpec::FixedBinary`] protocols.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeclarativeBinaryField {
    pub name: String,
    pub scalar_type: String,
    #[serde(default = "default_little_str")]
    pub endian: String,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub scale_param: Option<String>,
    #[serde(default)]
    pub offset: Option<f64>,
    #[serde(default)]
    pub offset_param: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}

/// Channel description for declarative [`DeclarativeProtocolSpec::TailMagic32Bit`] protocols.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclarativeTypedChannel {
    pub name: String,
    #[serde(default = "default_float_str")]
    pub data_type: String,
}

/// Declarative zero-compile protocol framing definitions mapped onto built-in codecs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "framing", rename_all = "snake_case")]
pub enum DeclarativeProtocolSpec {
    FixedBinary {
        header_hex: String,
        #[serde(default)]
        tail_hex: Option<String>,
        #[serde(default)]
        has_hw_tick_first_col: bool,
        #[serde(default = "default_none_str")]
        checksum: String,
        #[serde(default = "default_payload_only_str")]
        checksum_scope: String,
        #[serde(default = "default_little_str")]
        checksum_endian: String,
        fields: Vec<DeclarativeBinaryField>,
    },
    TailMagic32Bit {
        #[serde(default)]
        channels: Vec<DeclarativeTypedChannel>,
        #[serde(default)]
        channels_param: Option<String>,
    },
    CsvLine {
        #[serde(default)]
        channel_names: Vec<String>,
        #[serde(default)]
        channels_param: Option<String>,
    },
}

/// Shared library name specification inside a [`ProtocolPluginBackend::Cdylib`] descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlatformLibrarySpec {
    Stem(String),
    PerOs {
        #[serde(default)]
        windows: Option<String>,
        #[serde(default)]
        linux: Option<String>,
        #[serde(default)]
        macos: Option<String>,
    },
}

impl PlatformLibrarySpec {
    /// Resolves the dynamic library filename expected on the current compilation target OS.
    pub fn filename_for_current_os(&self) -> Option<String> {
        match self {
            Self::Stem(stem) => {
                let trimmed = stem.trim();
                if trimmed.is_empty() {
                    return None;
                }
                if trimmed.ends_with(".dll") || trimmed.ends_with(".so") || trimmed.ends_with(".dylib") {
                    return Some(trimmed.to_string());
                }
                #[cfg(target_os = "windows")]
                {
                    Some(format!("{trimmed}.dll"))
                }
                #[cfg(target_os = "macos")]
                {
                    Some(format!("lib{trimmed}.dylib"))
                }
                #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
                {
                    Some(format!("lib{trimmed}.so"))
                }
            }
            Self::PerOs {
                windows,
                linux,
                macos,
            } => {
                let _ = (windows, linux, macos);
                #[cfg(target_os = "windows")]
                {
                    windows.clone()
                }
                #[cfg(target_os = "macos")]
                {
                    macos.clone()
                }
                #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
                {
                    linux.clone()
                }
            }
        }
    }
}

/// Execution backend for an external protocol plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProtocolPluginBackend {
    Declarative(DeclarativeProtocolSpec),
    Cdylib {
        library: PlatformLibrarySpec,
        #[serde(default)]
        symbol: Option<String>,
    },
}

impl ProtocolPluginBackend {
    /// Returns the short mode tag (`"declarative"` or `"cdylib"`).
    pub const fn mode_tag(&self) -> &'static str {
        match self {
            Self::Declarative(_) => "declarative",
            Self::Cdylib { .. } => "cdylib",
        }
    }
}

/// Top-level `plugin.json` descriptor for external data engine (protocol) plugins.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProtocolPluginManifest {
    pub schema: String,
    pub id: String,
    pub name: LocalizedText,
    pub version: String,
    #[serde(default)]
    pub description: Option<LocalizedText>,
    #[serde(default)]
    pub params: Vec<PluginParamSpec>,
    #[serde(default)]
    pub channels_preview: Vec<PluginChannelPreview>,
    pub backend: ProtocolPluginBackend,
}

/// Channel binding expectation for an external widget plugin.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetBindingMode {
    #[default]
    SingleChannel,
    MultiChannel,
    None,
}

impl WidgetBindingMode {
    /// Returns the lowercase wire identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleChannel => "single_channel",
            Self::MultiChannel => "multi_channel",
            Self::None => "none",
        }
    }
}

const fn default_col_span() -> u32 {
    8
}

const fn default_row_span() -> u32 {
    4
}

fn default_entry_js() -> String {
    "index.js".to_string()
}

/// Top-level `plugin.json` descriptor for external dashboard widget plugins.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WidgetPluginManifest {
    pub schema: String,
    pub id: String,
    pub name: LocalizedText,
    pub version: String,
    #[serde(default)]
    pub description: Option<LocalizedText>,
    #[serde(default)]
    pub badge: Option<String>,
    #[serde(default = "default_col_span")]
    pub default_col_span: u32,
    #[serde(default = "default_row_span")]
    pub default_row_span: u32,
    #[serde(default)]
    pub binding_mode: WidgetBindingMode,
    #[serde(default = "default_entry_js")]
    pub entry_js: String,
    #[serde(default)]
    pub style_css: Option<String>,
    #[serde(default)]
    pub params: Vec<PluginParamSpec>,
}
