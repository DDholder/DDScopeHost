//! Static protocol codec factory registry decoupling instantiation from concrete codecs.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::ProtocolCodecFactory;
use crate::firewater::{FireWaterConfig, FireWaterCsvCodec};
use crate::fixed_binary::{
    ChecksumKind, ChecksumScope, FixedFieldSpec, FixedSchemaBinaryCodec, FixedSchemaBinaryConfig,
    SimPresetCodec, SimPresetConfig,
};
use crate::justfloat::{JustFloatCodec, JustFloatConfig, JustTypedCodec, JustTypedConfig};
use crate::ProtocolCodec;
use ddscope_types::{CoreError, CoreResult, Endianness, LinearScale, ScalarType, StateEnumMap};

/// Dynamic registry mapping protocol codec identifiers to [`ProtocolCodecFactory`] builders.
#[derive(Default)]
pub struct ProtocolRegistry {
    factories: HashMap<String, ProtocolCodecFactory>,
}

impl std::fmt::Debug for ProtocolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<&str> = self.factories.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("ProtocolRegistry")
            .field("registered_codecs", &names)
            .finish()
    }
}

impl ProtocolRegistry {
    /// Creates an empty [`ProtocolRegistry`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a [`ProtocolRegistry`] pre-populated with all built-in codecs:
    /// `"sim_preset"`, `"just_float"`, `"just_typed"`, `"fixed_schema_binary"`, and `"fire_water"`.
    pub fn with_builtin_codecs() -> Self {
        let mut reg = Self::new();
        reg.register("sim_preset", create_sim_preset_codec);
        reg.register("just_float", create_just_float_codec);
        reg.register("just_typed", create_just_typed_codec);
        reg.register("fixed_schema_binary", create_fixed_schema_binary_codec);
        reg.register("fire_water", create_fire_water_codec);
        reg
    }

    /// Registers or replaces the factory function or closure for `codec_id`.
    pub fn register<F>(&mut self, codec_id: impl Into<String>, factory: F)
    where
        F: Fn(&str) -> CoreResult<Box<dyn ProtocolCodec>> + Send + Sync + 'static,
    {
        self.factories.insert(codec_id.into(), Arc::new(factory));
    }

    /// Removes any factory registered for `codec_id`.
    pub fn unregister(&mut self, codec_id: &str) -> Option<ProtocolCodecFactory> {
        self.factories.remove(codec_id)
    }

    /// Returns `true` if a factory is registered for `codec_id`.
    #[inline]
    pub fn contains(&self, codec_id: &str) -> bool {
        self.factories.contains_key(codec_id)
    }

    /// Returns all currently registered codec identifiers in sorted order.
    pub fn registered_codecs(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.factories.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    /// Instantiates a [`ProtocolCodec`] identified by `codec_id` from serialized JSON config.
    pub fn create(&self, codec_id: &str, json_config: &str) -> CoreResult<Box<dyn ProtocolCodec>> {
        let factory = self.factories.get(codec_id).ok_or_else(|| {
            CoreError::Unsupported(format!("no protocol codec registered for `{codec_id}`"))
        })?;
        factory(json_config)
    }
}

fn parse_scalar_type_str(s: &str) -> CoreResult<ScalarType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "bool" => Ok(ScalarType::Bool),
        "u8" => Ok(ScalarType::U8),
        "u16" => Ok(ScalarType::U16),
        "u32" => Ok(ScalarType::U32),
        "u64" => Ok(ScalarType::U64),
        "i8" => Ok(ScalarType::I8),
        "i16" => Ok(ScalarType::I16),
        "i32" => Ok(ScalarType::I32),
        "i64" => Ok(ScalarType::I64),
        "f32" => Ok(ScalarType::F32),
        "f64" => Ok(ScalarType::F64),
        other => Err(CoreError::InvalidConfig(format!(
            "unknown scalar_type `{other}`"
        ))),
    }
}

#[derive(Deserialize)]
struct DtoFixedFieldInput {
    name: String,
    scalar_type: String,
    #[serde(default = "default_one")]
    scale: f64,
    #[serde(default)]
    offset: f64,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    is_state_enum: bool,
}

const fn default_one() -> f64 {
    1.0
}

fn default_dto_source_id() -> String {
    "fixed_binary".to_string()
}

fn default_max_carry() -> usize {
    64 * 1024
}

#[derive(Deserialize)]
struct DtoFixedSchemaBinaryInput {
    #[serde(default = "default_dto_source_id")]
    source_id: String,
    #[serde(default)]
    header_magic: Vec<u8>,
    #[serde(default)]
    tail_magic: Option<Vec<u8>>,
    #[serde(default)]
    checksum: ChecksumKind,
    #[serde(default)]
    checksum_scope: ChecksumScope,
    #[serde(default)]
    checksum_endian: Endianness,
    #[serde(default)]
    has_hw_tick_first_col: bool,
    fields: Vec<DtoFixedFieldInput>,
    #[serde(default = "default_max_carry")]
    max_carry_bytes: usize,
}

/// Factory function for `"sim_preset"`.
pub fn create_sim_preset_codec(json_config: &str) -> CoreResult<Box<dyn ProtocolCodec>> {
    let trimmed = json_config.trim();
    let cfg = if trimmed.is_empty() {
        SimPresetConfig::default()
    } else {
        serde_json::from_str::<SimPresetConfig>(trimmed)
            .map_err(|e| CoreError::InvalidConfig(format!("invalid SimPreset config: {e}")))?
    };
    Ok(Box::new(SimPresetCodec::new(cfg)?))
}

/// Factory function for `"just_float"`.
pub fn create_just_float_codec(json_config: &str) -> CoreResult<Box<dyn ProtocolCodec>> {
    let cfg: JustFloatConfig = serde_json::from_str(json_config)
        .map_err(|e| CoreError::InvalidConfig(format!("invalid JustFloat config: {e}")))?;
    Ok(Box::new(JustFloatCodec::new(cfg)?))
}

/// Factory function for `"just_typed"`.
pub fn create_just_typed_codec(json_config: &str) -> CoreResult<Box<dyn ProtocolCodec>> {
    let cfg: JustTypedConfig = serde_json::from_str(json_config)
        .map_err(|e| CoreError::InvalidConfig(format!("invalid JustTyped config: {e}")))?;
    Ok(Box::new(JustTypedCodec::new(cfg)?))
}

/// Factory function for `"fixed_schema_binary"` (accepts both [`FixedSchemaBinaryConfig`] and
/// control-plane `ProtocolConfigDto::FixedSchemaBinary` JSON shapes).
pub fn create_fixed_schema_binary_codec(
    json_config: &str,
) -> CoreResult<Box<dyn ProtocolCodec>> {
    let cfg = match serde_json::from_str::<FixedSchemaBinaryConfig>(json_config) {
        Ok(typed) => typed,
        Err(primary_err) => {
            let dto: DtoFixedSchemaBinaryInput =
                serde_json::from_str(json_config).map_err(|_| {
                    CoreError::InvalidConfig(format!(
                        "invalid FixedSchemaBinary config: {primary_err}"
                    ))
                })?;
            let fields = dto
                .fields
                .into_iter()
                .map(|f| {
                    let st = parse_scalar_type_str(&f.scalar_type)?;
                    let mut spec = if f.is_state_enum {
                        FixedFieldSpec::state_enum(f.name, st, StateEnumMap::new())
                    } else {
                        FixedFieldSpec::scalar(f.name, st)
                            .with_scale(LinearScale::new(f.scale, f.offset))
                    };
                    if let Some(u) = f.unit {
                        spec = spec.with_unit(u);
                    }
                    Ok(spec)
                })
                .collect::<CoreResult<Vec<_>>>()?;
            FixedSchemaBinaryConfig {
                source_id: dto.source_id,
                header_magic: dto.header_magic,
                tail_magic: dto.tail_magic,
                checksum: dto.checksum,
                checksum_scope: dto.checksum_scope,
                checksum_endian: dto.checksum_endian,
                has_hw_tick_first_col: dto.has_hw_tick_first_col,
                fields,
                max_carry_bytes: dto.max_carry_bytes,
            }
        }
    };
    Ok(Box::new(FixedSchemaBinaryCodec::new(cfg)?))
}

/// Factory function for `"fire_water"`.
pub fn create_fire_water_codec(json_config: &str) -> CoreResult<Box<dyn ProtocolCodec>> {
    let cfg: FireWaterConfig = serde_json::from_str(json_config)
        .map_err(|e| CoreError::InvalidConfig(format!("invalid FireWater config: {e}")))?;
    Ok(Box::new(FireWaterCsvCodec::new(cfg)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_creates_all_four_codecs() {
        let reg = ProtocolRegistry::with_builtin_codecs();
        assert_eq!(
            reg.registered_codecs(),
            vec![
                "fire_water",
                "fixed_schema_binary",
                "just_float",
                "just_typed",
                "sim_preset"
            ]
        );

        let sim = reg.create("sim_preset", "{}").unwrap();
        assert_eq!(sim.protocol_name(), "SimPreset");

        let jf = reg
            .create("just_float", r#"{"channel_names":["ia","ib"]}"#)
            .unwrap();
        assert_eq!(jf.protocol_name(), "JustFloat");

        let jt = reg
            .create(
                "just_typed",
                r#"{"channels":[{"name":"vbus","data_type":"float"},{"name":"cnt","data_type":"int"},{"name":"flags","data_type":"uint"}]}"#,
            )
            .unwrap();
        assert_eq!(jt.protocol_name(), "JustTyped");
        assert_eq!(jt.declared_channels().len(), 3);

        let fw = reg
            .create("fire_water", r#"{"channel_names":["ch0","ch1"]}"#)
            .unwrap();
        assert_eq!(fw.protocol_name(), "FireWater");

        let fb = reg
            .create(
                "fixed_schema_binary",
                r#"{"header_magic":[170,85],"has_hw_tick_first_col":true,"fields":[{"name":"tick","scalar_type":"u32","scale":1.0,"offset":0.0,"unit":null,"is_state_enum":false},{"name":"vbus","scalar_type":"f32","scale":1.0,"offset":0.0,"unit":"V","is_state_enum":false}]}"#,
            )
            .unwrap();
        assert_eq!(fb.protocol_name(), "FixedSchemaBinary");
        assert_eq!(fb.declared_channels().len(), 1);
    }
}
