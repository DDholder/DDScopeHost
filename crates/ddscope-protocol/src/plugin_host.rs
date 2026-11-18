//! External descriptor (`plugin.json`) and C-ABI (`cdylib`) protocol/widget plugin host.

use std::collections::HashSet;
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::time::SystemTime;

use bytes::BytesMut;
use ddscope_plugin_api::{
    is_safe_relative_asset_path, is_valid_plugin_id, CBatchSink, CByteSlice, CChannelDesc,
    CChannelSink, CErrorSink, CScalarType, DeclarativeProtocolSpec, PlatformLibrarySpec,
    PluginParamSpec, ProtocolPluginBackend, ProtocolPluginEntryFnV1, ProtocolPluginManifest,
    ProtocolPluginVTableV1, WidgetPluginManifest, DDSCOPE_C_ABI_VERSION,
    DDSCOPE_PROTOCOL_SYMBOL_V1, PROTOCOL_PLUGIN_SCHEMA_V1, WIDGET_PLUGIN_SCHEMA_V1,
};
use ddscope_types::{
    ChannelDescriptor, CoreError, CoreResult, DecodedBatch, DecodedColumnBuf, Endianness,
    LinearScale, ScalarType, TimestampNs,
};
use libloading::Library;
use serde_json::{Map, Value};
use tracing::{debug, warn};

use crate::downlink::encode_downlink_default;
use crate::firewater::{FireWaterConfig, FireWaterCsvCodec};
use crate::fixed_binary::{
    ChecksumKind, ChecksumScope, FixedFieldSpec, FixedSchemaBinaryCodec, FixedSchemaBinaryConfig,
};
use crate::justfloat::{JustTypedChannelKind, JustTypedChannelSpec, JustTypedCodec, JustTypedConfig};
use crate::registry::ProtocolRegistry;
use crate::{DownlinkPayload, ProtocolCodec};

const MAX_CDYLIB_CARRY_BYTES: usize = 64 * 1024;
const BUILTIN_CODEC_IDS: &[&str] = &[
    "sim_preset",
    "just_float",
    "just_typed",
    "fixed_schema_binary",
    "fire_water",
];

/// Metadata and availability state for a discovered external protocol plugin.
#[derive(Clone, Debug)]
pub struct ExternalProtocolPluginEntry {
    pub manifest: ProtocolPluginManifest,
    pub plugin_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub available: bool,
    pub error: Option<String>,
}

/// Metadata and availability state for a discovered external widget plugin.
#[derive(Clone, Debug)]
pub struct ExternalWidgetPluginEntry {
    pub manifest: WidgetPluginManifest,
    pub plugin_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub entry_js_path: PathBuf,
    pub style_css_path: Option<PathBuf>,
    pub available: bool,
    pub error: Option<String>,
}

/// UTF-8 JavaScript module + optional CSS stylesheet loaded for an external widget plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedWidgetBundle {
    pub id: String,
    pub entry_js: String,
    pub style_css: Option<String>,
}

/// Snapshot of all external protocol and widget plugins discovered on disk.
#[derive(Clone, Debug, Default)]
pub struct ExternalPluginCatalog {
    pub search_dirs: Vec<PathBuf>,
    pub protocols: Vec<ExternalProtocolPluginEntry>,
    pub widgets: Vec<ExternalWidgetPluginEntry>,
}

/// Resolves the ordered list of `plugins/` directories to scan.
///
/// When `DDSCOPE_PLUGINS_DIR` is set and non-empty, ONLY those path entries are used.
/// Otherwise scans `<exe_dir>/plugins` and `<cwd>[/../..]/plugins`.
pub fn resolve_plugin_search_dirs() -> Vec<PathBuf> {
    if let Some(raw_env) = std::env::var_os("DDSCOPE_PLUGINS_DIR") {
        if !raw_env.is_empty() {
            let mut dirs = Vec::new();
            let mut seen = HashSet::new();
            for candidate in std::env::split_paths(&raw_env) {
                if candidate.as_os_str().is_empty() {
                    continue;
                }
                let key = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
                if seen.insert(key) {
                    dirs.push(candidate);
                }
            }
            if !dirs.is_empty() {
                return dirs;
            }
        }
    }

    let mut candidates = Vec::new();
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            candidates.push(exe_dir.join("plugins"));
            if let Some(exe_parent) = exe_dir.parent() {
                candidates.push(exe_parent.join("plugins"));
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let mut cur: Option<&Path> = Some(cwd.as_path());
        for _ in 0..4 {
            if let Some(dir) = cur {
                candidates.push(dir.join("plugins"));
                cur = dir.parent();
            } else {
                break;
            }
        }
    }

    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    for c in candidates {
        if c.is_dir() {
            let key = fs::canonicalize(&c).unwrap_or_else(|_| c.clone());
            if seen.insert(key) {
                dirs.push(c);
            }
        }
    }
    dirs
}

/// Scans `dirs` for protocol and widget plugins without mutating any registry.
pub fn discover_external_plugins(dirs: &[PathBuf]) -> ExternalPluginCatalog {
    discover_plugins_internal(dirs, None)
}

/// Scans `dirs` for external protocol and widget plugins and registers ready protocol codecs
/// into `registry`.
pub fn discover_and_register_external_plugins(
    registry: &mut ProtocolRegistry,
    dirs: &[PathBuf],
) -> ExternalPluginCatalog {
    discover_plugins_internal(dirs, Some(registry))
}

/// Reads the ESM bundle (`index.js` and optional `style.css`) for widget plugin `plugin_id`
/// with strict path-traversal containment checks.
pub fn read_widget_plugin_bundle(
    catalog: &ExternalPluginCatalog,
    plugin_id: &str,
) -> CoreResult<LoadedWidgetBundle> {
    if !is_valid_plugin_id(plugin_id) {
        return Err(CoreError::InvalidConfig(format!(
            "invalid widget plugin id `{plugin_id}`"
        )));
    }

    let entry = catalog
        .widgets
        .iter()
        .find(|w| w.manifest.id == plugin_id)
        .ok_or_else(|| {
            CoreError::InvalidConfig(format!("external widget plugin `{plugin_id}` not found"))
        })?;

    if !entry.available {
        return Err(CoreError::InvalidConfig(format!(
            "widget plugin `{plugin_id}` is unavailable: {}",
            entry.error.as_deref().unwrap_or("unknown error")
        )));
    }

    let dir_canon = fs::canonicalize(&entry.plugin_dir).map_err(|e| {
        CoreError::InvalidConfig(format!(
            "failed to canonicalize widget plugin dir `{}`: {e}",
            entry.plugin_dir.display()
        ))
    })?;
    let entry_canon = fs::canonicalize(&entry.entry_js_path).map_err(|e| {
        CoreError::InvalidConfig(format!(
            "failed to resolve widget entry script `{}`: {e}",
            entry.entry_js_path.display()
        ))
    })?;
    if !entry_canon.starts_with(&dir_canon) {
        return Err(CoreError::InvalidConfig(format!(
            "widget plugin `{plugin_id}` entry script escapes plugin directory"
        )));
    }
    let entry_js = fs::read_to_string(&entry_canon).map_err(|e| {
        CoreError::InvalidConfig(format!(
            "failed to read widget plugin script `{}`: {e}",
            entry_canon.display()
        ))
    })?;

    let style_css = match &entry.style_css_path {
        Some(style_path) => {
            let style_canon = fs::canonicalize(style_path).map_err(|e| {
                CoreError::InvalidConfig(format!(
                    "failed to resolve widget stylesheet `{}`: {e}",
                    style_path.display()
                ))
            })?;
            if !style_canon.starts_with(&dir_canon) {
                return Err(CoreError::InvalidConfig(format!(
                    "widget plugin `{plugin_id}` stylesheet escapes plugin directory"
                )));
            }
            Some(fs::read_to_string(&style_canon).map_err(|e| {
                CoreError::InvalidConfig(format!(
                    "failed to read widget stylesheet `{}`: {e}",
                    style_canon.display()
                ))
            })?)
        }
        None => None,
    };

    Ok(LoadedWidgetBundle {
        id: plugin_id.to_string(),
        entry_js,
        style_css,
    })
}

fn discover_plugins_internal(
    dirs: &[PathBuf],
    mut registry: Option<&mut ProtocolRegistry>,
) -> ExternalPluginCatalog {
    let mut catalog = ExternalPluginCatalog {
        search_dirs: dirs.to_vec(),
        protocols: Vec::new(),
        widgets: Vec::new(),
    };
    let mut seen_proto_ids = HashSet::new();
    let mut seen_widget_ids = HashSet::new();

    for root in dirs {
        let proto_root = root.join("protocols");
        if proto_root.is_dir() {
            for plugin_dir in sorted_subdirs(&proto_root) {
                let manifest_path = plugin_dir.join("plugin.json");
                if !manifest_path.is_file() {
                    continue;
                }
                if let Some(entry) = load_protocol_entry(&plugin_dir, &manifest_path) {
                    if !seen_proto_ids.insert(entry.manifest.id.clone()) {
                        continue;
                    }
                    let mut entry = entry;
                    if entry.available {
                        match build_protocol_factory(&entry.plugin_dir, &entry.manifest) {
                            Ok(factory) => {
                                if let Some(reg) = registry.as_deref_mut() {
                                    let id = entry.manifest.id.clone();
                                    reg.register(id, move |cfg| factory(cfg));
                                }
                                debug!(
                                    plugin_id = %entry.manifest.id,
                                    mode = entry.manifest.backend.mode_tag(),
                                    "registered external protocol plugin"
                                );
                            }
                            Err(err) => {
                                warn!(
                                    plugin_id = %entry.manifest.id,
                                    error = %err,
                                    "failed initializing external protocol plugin backend"
                                );
                                entry.available = false;
                                entry.error = Some(err.to_string());
                            }
                        }
                    }
                    catalog.protocols.push(entry);
                }
            }
        }

        let widget_root = root.join("widgets");
        if widget_root.is_dir() {
            for plugin_dir in sorted_subdirs(&widget_root) {
                let manifest_path = plugin_dir.join("plugin.json");
                if !manifest_path.is_file() {
                    continue;
                }
                if let Some(entry) = load_widget_entry(&plugin_dir, &manifest_path) {
                    if !seen_widget_ids.insert(entry.manifest.id.clone()) {
                        continue;
                    }
                    catalog.widgets.push(entry);
                }
            }
        }
    }

    catalog
}

fn sorted_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn load_protocol_entry(
    plugin_dir: &Path,
    manifest_path: &Path,
) -> Option<ExternalProtocolPluginEntry> {
    let raw = match fs::read_to_string(manifest_path) {
        Ok(s) => s,
        Err(e) => {
            warn!(path = %manifest_path.display(), error = %e, "failed reading protocol plugin.json");
            return None;
        }
    };
    let manifest: ProtocolPluginManifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            warn!(path = %manifest_path.display(), error = %e, "invalid protocol plugin.json");
            return None;
        }
    };

    let mut available = true;
    let mut error = None;

    if manifest.schema != PROTOCOL_PLUGIN_SCHEMA_V1 {
        available = false;
        error = Some(format!(
            "unsupported schema `{}` (expected `{PROTOCOL_PLUGIN_SCHEMA_V1}`)",
            manifest.schema
        ));
    } else if !is_valid_plugin_id(&manifest.id) {
        available = false;
        error = Some(format!("invalid plugin id `{}`", manifest.id));
    } else if BUILTIN_CODEC_IDS.contains(&manifest.id.as_str()) {
        available = false;
        error = Some(format!(
            "external plugin id `{}` collides with built-in codec",
            manifest.id
        ));
    }

    Some(ExternalProtocolPluginEntry {
        manifest,
        plugin_dir: plugin_dir.to_path_buf(),
        manifest_path: manifest_path.to_path_buf(),
        available,
        error,
    })
}

fn load_widget_entry(
    plugin_dir: &Path,
    manifest_path: &Path,
) -> Option<ExternalWidgetPluginEntry> {
    let raw = match fs::read_to_string(manifest_path) {
        Ok(s) => s,
        Err(e) => {
            warn!(path = %manifest_path.display(), error = %e, "failed reading widget plugin.json");
            return None;
        }
    };
    let manifest: WidgetPluginManifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            warn!(path = %manifest_path.display(), error = %e, "invalid widget plugin.json");
            return None;
        }
    };

    let mut available = true;
    let mut error = None;

    if manifest.schema != WIDGET_PLUGIN_SCHEMA_V1 {
        available = false;
        error = Some(format!(
            "unsupported schema `{}` (expected `{WIDGET_PLUGIN_SCHEMA_V1}`)",
            manifest.schema
        ));
    } else if !is_valid_plugin_id(&manifest.id) {
        available = false;
        error = Some(format!("invalid widget plugin id `{}`", manifest.id));
    } else if !is_safe_relative_asset_path(&manifest.entry_js) {
        available = false;
        error = Some(format!("unsafe entry_js path `{}`", manifest.entry_js));
    } else if let Some(style_rel) = &manifest.style_css {
        if !is_safe_relative_asset_path(style_rel) {
            available = false;
            error = Some(format!("unsafe style_css path `{style_rel}`"));
        }
    }

    let entry_js_path = plugin_dir.join(manifest.entry_js.trim());
    let style_css_path = manifest
        .style_css
        .as_ref()
        .map(|rel| plugin_dir.join(rel.trim()));

    if available {
        if !entry_js_path.is_file() {
            available = false;
            error = Some(format!(
                "missing widget entry script `{}`",
                entry_js_path.display()
            ));
        } else if let Some(style_p) = &style_css_path {
            if !style_p.is_file() {
                available = false;
                error = Some(format!("missing widget stylesheet `{}`", style_p.display()));
            }
        }
    }

    Some(ExternalWidgetPluginEntry {
        manifest,
        plugin_dir: plugin_dir.to_path_buf(),
        manifest_path: manifest_path.to_path_buf(),
        entry_js_path,
        style_css_path,
        available,
        error,
    })
}

fn build_protocol_factory(
    plugin_dir: &Path,
    manifest: &ProtocolPluginManifest,
) -> CoreResult<Arc<dyn Fn(&str) -> CoreResult<Box<dyn ProtocolCodec>> + Send + Sync>> {
    match &manifest.backend {
        ProtocolPluginBackend::Declarative(spec) => {
            build_declarative_codec_factory(manifest, spec.clone())
        }
        ProtocolPluginBackend::Cdylib { library, symbol } => {
            load_cdylib_protocol_factory(plugin_dir, manifest, library, symbol.as_deref())
        }
    }
}

fn merge_manifest_params_json(
    params: &[PluginParamSpec],
    config_json: &str,
) -> CoreResult<(Map<String, Value>, String)> {
    let trimmed = config_json.trim();
    let mut map = if trimmed.is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Value>(trimmed) {
            Ok(Value::Object(obj)) => obj,
            Ok(Value::Null) => Map::new(),
            Ok(_) => {
                return Err(CoreError::InvalidConfig(
                    "plugin config_json must be a JSON object".into(),
                ));
            }
            Err(e) => {
                return Err(CoreError::InvalidConfig(format!(
                    "invalid plugin config_json: {e}"
                )));
            }
        }
    };

    for p in params {
        if !map.contains_key(&p.key) && !p.default_value.is_null() {
            map.insert(p.key.clone(), p.default_value.clone());
        }
    }

    let serialized = Value::Object(map.clone()).to_string();
    Ok((map, serialized))
}

struct NamedCodecAdapter {
    protocol_name: String,
    inner: Box<dyn ProtocolCodec>,
}

impl ProtocolCodec for NamedCodecAdapter {
    fn protocol_name(&self) -> &str {
        &self.protocol_name
    }

    fn declared_channels(&self) -> &[ChannelDescriptor] {
        self.inner.declared_channels()
    }

    fn declared_schemas(&self) -> &[Arc<ddscope_types::StructSchema>] {
        self.inner.declared_schemas()
    }

    fn feed_bytes(
        &mut self,
        chunk: &[u8],
        recv_ns: TimestampNs,
        batch_out: &mut DecodedBatch,
    ) -> CoreResult<usize> {
        self.inner.feed_bytes(chunk, recv_ns, batch_out)
    }

    fn reset_state(&mut self) {
        self.inner.reset_state();
    }

    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        self.inner.encode_downlink(req, out)
    }
}

fn build_declarative_codec_factory(
    manifest: &ProtocolPluginManifest,
    spec: DeclarativeProtocolSpec,
) -> CoreResult<Arc<dyn Fn(&str) -> CoreResult<Box<dyn ProtocolCodec>> + Send + Sync>> {
    // Eagerly validate static fields at load time so malformed declarative plugin.json is flagged immediately.
    let _ = instantiate_declarative_codec(&manifest.id, &manifest.params, &spec, "{}")?;

    let plugin_id = manifest.id.clone();
    let params = manifest.params.clone();
    Ok(Arc::new(move |config_json: &str| {
        instantiate_declarative_codec(&plugin_id, &params, &spec, config_json)
    }))
}

fn instantiate_declarative_codec(
    plugin_id: &str,
    params: &[PluginParamSpec],
    spec: &DeclarativeProtocolSpec,
    config_json: &str,
) -> CoreResult<Box<dyn ProtocolCodec>> {
    let (merged, _) = merge_manifest_params_json(params, config_json)?;
    let inner: Box<dyn ProtocolCodec> = match spec {
        DeclarativeProtocolSpec::FixedBinary {
            header_hex,
            tail_hex,
            has_hw_tick_first_col,
            checksum,
            checksum_scope,
            checksum_endian,
            fields,
        } => {
            let header_magic = parse_hex_token_bytes(header_hex)?;
            let tail_magic = match tail_hex.as_deref() {
                Some(t) if !t.trim().is_empty() => Some(parse_hex_token_bytes(t)?),
                _ => None,
            };
            let checksum_kind = parse_checksum_kind(checksum)?;
            let scope = parse_checksum_scope(checksum_scope)?;
            let c_endian = parse_endianness(checksum_endian)?;

            let mut parsed_fields = Vec::with_capacity(fields.len());
            for f in fields {
                let scalar_type = parse_scalar_type_str(&f.scalar_type)?;
                let endian = parse_endianness(&f.endian)?;
                let scale_val = match &f.scale_param {
                    Some(k) => match merged.get(k) {
                        Some(v) => value_as_finite_f64(v, k)?,
                        None => f.scale.unwrap_or(1.0),
                    },
                    None => f.scale.unwrap_or(1.0),
                };
                let offset_val = match &f.offset_param {
                    Some(k) => match merged.get(k) {
                        Some(v) => value_as_finite_f64(v, k)?,
                        None => f.offset.unwrap_or(0.0),
                    },
                    None => f.offset.unwrap_or(0.0),
                };
                let mut field = FixedFieldSpec::scalar(f.name.clone(), scalar_type)
                    .with_endian(endian)
                    .with_scale(LinearScale::new(scale_val, offset_val));
                if let Some(u) = &f.unit {
                    if !u.trim().is_empty() {
                        field = field.with_unit(u.trim());
                    }
                }
                parsed_fields.push(field);
            }

            let cfg = FixedSchemaBinaryConfig {
                source_id: plugin_id.to_string(),
                header_magic,
                tail_magic,
                checksum: checksum_kind,
                checksum_scope: scope,
                checksum_endian: c_endian,
                has_hw_tick_first_col: *has_hw_tick_first_col,
                fields: parsed_fields,
                max_carry_bytes: 64 * 1024,
            };
            Box::new(FixedSchemaBinaryCodec::new(cfg)?)
        }
        DeclarativeProtocolSpec::TailMagic32Bit {
            channels,
            channels_param,
        } => {
            let mut specs = Vec::new();
            if let Some(param_key) = channels_param {
                if let Some(val) = merged.get(param_key) {
                    specs = parse_typed_channels_value(val)?;
                }
            }
            if specs.is_empty() {
                for ch in channels {
                    specs.push(JustTypedChannelSpec::new(
                        ch.name.clone(),
                        parse_just_typed_kind(&ch.data_type)?,
                    ));
                }
            }
            Box::new(JustTypedCodec::new(JustTypedConfig::new(plugin_id, specs))?)
        }
        DeclarativeProtocolSpec::CsvLine {
            channel_names,
            channels_param,
        } => {
            let mut names = Vec::new();
            if let Some(param_key) = channels_param {
                if let Some(val) = merged.get(param_key) {
                    names = parse_channel_names_value(val);
                }
            }
            if names.is_empty() {
                names = channel_names.clone();
            }
            Box::new(FireWaterCsvCodec::new(FireWaterConfig::from_channel_names(
                plugin_id, names,
            ))?)
        }
    };

    Ok(Box::new(NamedCodecAdapter {
        protocol_name: plugin_id.to_string(),
        inner,
    }))
}

#[derive(Copy, Clone)]
struct CdylibVTable(ProtocolPluginVTableV1);

// SAFETY: The exported vtable only contains plain static function pointers and static
// string slices belonging to the loaded DLL, whose lifetime is bound via `Arc<Library>`.
unsafe impl Send for CdylibVTable {}
unsafe impl Sync for CdylibVTable {}

struct CdylibCodecWrapper {
    // SAFETY CRITICAL: `instance` MUST be declared before `_lib` so Rust drops
    // `instance` (calling `destroy_instance`) before `Arc<Library>` potentially unloads the DLL.
    instance: *mut c_void,
    vtable: CdylibVTable,
    channels: Vec<ChannelDescriptor>,
    protocol_name: String,
    carry: BytesMut,
    _lib: Arc<Library>,
}

// SAFETY: `CdylibCodecWrapper` owns its opaque `instance` exclusively (`&mut self` in `ProtocolCodec`),
// and is only moved between threads (never shared concurrently via `&`).
unsafe impl Send for CdylibCodecWrapper {}

impl Drop for CdylibCodecWrapper {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            unsafe {
                (self.vtable.0.destroy_instance)(self.instance);
            }
            self.instance = ptr::null_mut();
        }
    }
}

struct BatchSinkCtx<'a> {
    batch: &'a mut DecodedBatch,
    rows_decoded: usize,
    pending_hw_tick: Option<u64>,
    framing_errors: u64,
}

unsafe extern "C" fn cb_push_channel(ctx: *mut c_void, desc: CChannelDesc) {
    let out = unsafe { &mut *(ctx as *mut Vec<ChannelDescriptor>) };
    let name_bytes = unsafe { desc.name.as_slice() };
    let name = String::from_utf8_lossy(name_bytes).trim().to_string();
    if name.is_empty() {
        return;
    }
    let unit_bytes = unsafe { desc.unit.as_slice() };
    let unit_str = String::from_utf8_lossy(unit_bytes).trim().to_string();
    let scalar_type = match desc.scalar_type {
        CScalarType::F32 => ScalarType::F32,
        CScalarType::I32 => ScalarType::I32,
        CScalarType::U32 => ScalarType::U32,
        CScalarType::F64 => ScalarType::F64,
        CScalarType::Bool => ScalarType::Bool,
    };
    let mut cd = ChannelDescriptor::new_scalar("plugin", name, scalar_type);
    if !unit_str.is_empty() {
        cd.unit = Some(unit_str);
    }
    out.push(cd);
}

unsafe extern "C" fn cb_set_error(ctx: *mut c_void, msg: CByteSlice) {
    let err_out = unsafe { &mut *(ctx as *mut Option<String>) };
    let bytes = unsafe { msg.as_slice() };
    *err_out = Some(String::from_utf8_lossy(bytes).into_owned());
}

unsafe extern "C" fn cb_push_f32(ctx: *mut c_void, col_idx: usize, val: f32) {
    let st = unsafe { &mut *(ctx as *mut BatchSinkCtx<'_>) };
    if let Some(DecodedColumnBuf::F32(col)) = st.batch.columns.get_mut(col_idx) {
        col.push(val);
    }
}

unsafe extern "C" fn cb_push_i32(ctx: *mut c_void, col_idx: usize, val: i32) {
    let st = unsafe { &mut *(ctx as *mut BatchSinkCtx<'_>) };
    if let Some(DecodedColumnBuf::I32(col)) = st.batch.columns.get_mut(col_idx) {
        col.push(val);
    }
}

unsafe extern "C" fn cb_push_u32(ctx: *mut c_void, col_idx: usize, val: u32) {
    let st = unsafe { &mut *(ctx as *mut BatchSinkCtx<'_>) };
    if let Some(DecodedColumnBuf::U32(col)) = st.batch.columns.get_mut(col_idx) {
        col.push(val);
    }
}

unsafe extern "C" fn cb_push_f64(ctx: *mut c_void, col_idx: usize, val: f64) {
    let st = unsafe { &mut *(ctx as *mut BatchSinkCtx<'_>) };
    if let Some(DecodedColumnBuf::F64(col)) = st.batch.columns.get_mut(col_idx) {
        col.push(val);
    }
}

unsafe extern "C" fn cb_push_bool(ctx: *mut c_void, col_idx: usize, val: u8) {
    let st = unsafe { &mut *(ctx as *mut BatchSinkCtx<'_>) };
    if let Some(DecodedColumnBuf::Bool(col)) = st.batch.columns.get_mut(col_idx) {
        col.push(val != 0);
    }
}

unsafe extern "C" fn cb_push_hw_tick(ctx: *mut c_void, tick: u64) {
    let st = unsafe { &mut *(ctx as *mut BatchSinkCtx<'_>) };
    st.pending_hw_tick = Some(tick);
}

unsafe extern "C" fn cb_finish_row(ctx: *mut c_void) {
    let st = unsafe { &mut *(ctx as *mut BatchSinkCtx<'_>) };
    if let Some(tick) = st.pending_hw_tick.take() {
        st.batch.hw_ticks.push(tick);
    }
    st.rows_decoded += 1;
    st.batch.sample_rows += 1;
}

unsafe extern "C" fn cb_record_framing_error(ctx: *mut c_void, count: u64) {
    let st = unsafe { &mut *(ctx as *mut BatchSinkCtx<'_>) };
    st.framing_errors = st.framing_errors.saturating_add(count);
}

impl ProtocolCodec for CdylibCodecWrapper {
    fn protocol_name(&self) -> &str {
        &self.protocol_name
    }

    fn declared_channels(&self) -> &[ChannelDescriptor] {
        &self.channels
    }

    fn feed_bytes(
        &mut self,
        chunk: &[u8],
        recv_ns: TimestampNs,
        batch_out: &mut DecodedBatch,
    ) -> CoreResult<usize> {
        batch_out.packet_recv_ns = recv_ns;
        if self.carry.is_empty() && chunk.is_empty() {
            return Ok(0);
        }

        let using_carry = !self.carry.is_empty();
        if using_carry {
            self.carry.extend_from_slice(chunk);
        }

        let mut err_msg: Option<String> = None;
        let err_sink = CErrorSink {
            ctx: (&mut err_msg as *mut Option<String>).cast(),
            set_error: cb_set_error,
        };

        let mut sink_ctx = BatchSinkCtx {
            batch: batch_out,
            rows_decoded: 0,
            pending_hw_tick: None,
            framing_errors: 0,
        };
        let rows_decoded;
        let consumed_res: isize;
        let input_len: usize;

        {
            let slice: &[u8] = if using_carry { &self.carry } else { chunk };
            input_len = slice.len();
            let batch_sink = CBatchSink {
                ctx: (&mut sink_ctx as *mut BatchSinkCtx<'_>).cast(),
                push_f32: cb_push_f32,
                push_i32: cb_push_i32,
                push_u32: cb_push_u32,
                push_f64: cb_push_f64,
                push_bool: cb_push_bool,
                push_hw_tick: cb_push_hw_tick,
                finish_row: cb_finish_row,
                record_framing_error: cb_record_framing_error,
            };

            consumed_res = unsafe {
                (self.vtable.0.decode_chunk)(
                    self.instance,
                    CByteSlice::from_slice(slice),
                    0,
                    &batch_sink,
                    &err_sink,
                )
            };
            rows_decoded = sink_ctx.rows_decoded;
        }

        if consumed_res < 0 {
            self.carry.clear();
            return Err(CoreError::Protocol {
                message: err_msg.unwrap_or_else(|| {
                    format!("cdylib plugin `{}` returned decode error", self.protocol_name)
                }),
                recoverable: true,
            });
        }

        let consumed = (consumed_res as usize).min(input_len);
        if using_carry {
            if consumed == input_len {
                self.carry.clear();
            } else {
                let _ = self.carry.split_to(consumed);
                if self.carry.len() > MAX_CDYLIB_CARRY_BYTES {
                    let drop_len = self.carry.len() - MAX_CDYLIB_CARRY_BYTES;
                    let _ = self.carry.split_to(drop_len);
                }
            }
        } else if consumed < chunk.len() {
            let tail = &chunk[consumed..];
            let capped = if tail.len() > MAX_CDYLIB_CARRY_BYTES {
                &tail[tail.len() - MAX_CDYLIB_CARRY_BYTES..]
            } else {
                tail
            };
            self.carry.extend_from_slice(capped);
        }

        Ok(rows_decoded)
    }

    fn reset_state(&mut self) {
        self.carry.clear();
        unsafe {
            (self.vtable.0.reset_instance)(self.instance);
        }
    }

    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        encode_downlink_default(req, out)
    }
}

fn load_cdylib_protocol_factory(
    plugin_dir: &Path,
    manifest: &ProtocolPluginManifest,
    library: &PlatformLibrarySpec,
    symbol_override: Option<&str>,
) -> CoreResult<Arc<dyn Fn(&str) -> CoreResult<Box<dyn ProtocolCodec>> + Send + Sync>> {
    let lib_path = resolve_cdylib_path(plugin_dir, library)?;
    let lib = Arc::new(unsafe { Library::new(&lib_path) }.map_err(|e| {
        CoreError::InvalidConfig(format!(
            "failed loading dynamic library `{}`: {e}",
            lib_path.display()
        ))
    })?);

    let sym_bytes: Vec<u8> = match symbol_override.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => {
            let mut b = name.as_bytes().to_vec();
            b.push(0);
            b
        }
        None => DDSCOPE_PROTOCOL_SYMBOL_V1.to_vec(),
    };

    let vtable = unsafe {
        let sym = lib
            .get::<ProtocolPluginEntryFnV1>(&sym_bytes)
            .map_err(|e| {
                CoreError::InvalidConfig(format!(
                    "missing symbol `{}` in `{}`: {e}",
                    String::from_utf8_lossy(&sym_bytes).trim_end_matches('\0'),
                    lib_path.display()
                ))
            })?;
        sym()
    };

    if vtable.abi_version != DDSCOPE_C_ABI_VERSION {
        return Err(CoreError::Unsupported(format!(
            "cdylib plugin `{}` reports C-ABI v{}, expected v{DDSCOPE_C_ABI_VERSION}",
            manifest.id, vtable.abi_version
        )));
    }

    let vtable_handle = CdylibVTable(vtable);
    let params = manifest.params.clone();
    let plugin_id = manifest.id.clone();

    Ok(Arc::new(move |config_json: &str| {
        let (_, merged_json) = merge_manifest_params_json(&params, config_json)?;
        let mut err_msg: Option<String> = None;
        let err_sink = CErrorSink {
            ctx: (&mut err_msg as *mut Option<String>).cast(),
            set_error: cb_set_error,
        };
        let instance = unsafe {
            (vtable_handle.0.create_instance)(CByteSlice::from_str(&merged_json), &err_sink)
        };
        if instance.is_null() {
            return Err(CoreError::InvalidConfig(err_msg.unwrap_or_else(|| {
                format!("cdylib plugin `{plugin_id}` rejected config")
            })));
        }

        let mut channels = Vec::new();
        let ch_sink = CChannelSink {
            ctx: (&mut channels as *mut Vec<ChannelDescriptor>).cast(),
            push_channel: cb_push_channel,
        };
        unsafe {
            (vtable_handle.0.declare_channels)(instance, &ch_sink);
        }
        if channels.is_empty() {
            unsafe {
                (vtable_handle.0.destroy_instance)(instance);
            }
            return Err(CoreError::InvalidConfig(format!(
                "cdylib plugin `{plugin_id}` declared zero channels"
            )));
        }

        Ok(Box::new(CdylibCodecWrapper {
            instance,
            vtable: vtable_handle,
            channels,
            protocol_name: plugin_id.clone(),
            carry: BytesMut::with_capacity(4096),
            _lib: Arc::clone(&lib),
        }))
    }))
}

fn resolve_cdylib_path(plugin_dir: &Path, spec: &PlatformLibrarySpec) -> CoreResult<PathBuf> {
    let filename = spec.filename_for_current_os().ok_or_else(|| {
        CoreError::InvalidConfig("missing dynamic library filename for current OS".into())
    })?;
    if !is_safe_relative_asset_path(&filename) {
        return Err(CoreError::InvalidConfig(format!(
            "unsafe dynamic library filename `{filename}`"
        )));
    }

    let direct = plugin_dir.join(&filename);
    if direct.is_file() {
        return Ok(direct);
    }

    let mut fallback_candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            fallback_candidates.push(dir.join(&filename));
            fallback_candidates.push(dir.join("deps").join(&filename));
        }
    }

    let mut cur: Option<&Path> = Some(plugin_dir);
    for _ in 0..5 {
        if let Some(dir) = cur {
            fallback_candidates.push(dir.join("target").join("debug").join(&filename));
            fallback_candidates.push(dir.join("target").join("release").join(&filename));
            cur = dir.parent();
        } else {
            break;
        }
    }

    let mut best: Option<(SystemTime, PathBuf)> = None;
    for candidate in fallback_candidates {
        if candidate.is_file() {
            let mtime = fs::metadata(&candidate)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            match &best {
                Some((best_time, _)) if *best_time >= mtime => {}
                _ => best = Some((mtime, candidate)),
            }
        }
    }

    best.map(|(_, p)| p).ok_or_else(|| {
        CoreError::InvalidConfig(format!(
            "dynamic library `{filename}` not found in `{}` or workspace target/{{debug,release}}",
            plugin_dir.display()
        ))
    })
}

fn parse_hex_token_bytes(spec: &str) -> CoreResult<Vec<u8>> {
    let cleaned: String = spec
        .split_whitespace()
        .flat_map(|tok| {
            let stripped = tok
                .strip_prefix("0x")
                .or_else(|| tok.strip_prefix("0X"))
                .unwrap_or(tok);
            stripped.chars()
        })
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != ',')
        .collect();
    if cleaned.is_empty() {
        return Err(CoreError::InvalidConfig("hex sequence must not be empty".into()));
    }
    hex::decode(&cleaned)
        .map_err(|e| CoreError::InvalidConfig(format!("invalid hex sequence `{spec}`: {e}")))
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

fn parse_endianness(s: &str) -> CoreResult<Endianness> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "little" | "le" => Ok(Endianness::Little),
        "big" | "be" => Ok(Endianness::Big),
        other => Err(CoreError::InvalidConfig(format!(
            "unknown endianness `{other}`"
        ))),
    }
}

fn parse_checksum_kind(s: &str) -> CoreResult<ChecksumKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "none" => Ok(ChecksumKind::None),
        "sum8" => Ok(ChecksumKind::Sum8),
        "xor8" => Ok(ChecksumKind::Xor8),
        "crc16" | "crc16_ccitt" => Ok(ChecksumKind::Crc16Ccitt),
        "crc32" => Ok(ChecksumKind::Crc32),
        other => Err(CoreError::InvalidConfig(format!(
            "unknown checksum `{other}`"
        ))),
    }
}

fn parse_checksum_scope(s: &str) -> CoreResult<ChecksumScope> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "payload_only" => Ok(ChecksumScope::PayloadOnly),
        "header_and_payload" => Ok(ChecksumScope::HeaderAndPayload),
        other => Err(CoreError::InvalidConfig(format!(
            "unknown checksum_scope `{other}`"
        ))),
    }
}

fn value_as_finite_f64(val: &Value, key: &str) -> CoreResult<f64> {
    let parsed = match val {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    let Some(v) = parsed else {
        return Err(CoreError::InvalidConfig(format!(
            "plugin parameter `{key}` must be a finite number"
        )));
    };
    if !v.is_finite() {
        return Err(CoreError::InvalidConfig(format!(
            "plugin parameter `{key}` must be finite"
        )));
    }
    Ok(v)
}

fn parse_just_typed_kind(s: &str) -> CoreResult<JustTypedChannelKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "float" | "f32" => Ok(JustTypedChannelKind::Float),
        "int" | "i32" => Ok(JustTypedChannelKind::Int),
        "uint" | "u32" => Ok(JustTypedChannelKind::Uint),
        other => Err(CoreError::InvalidConfig(format!(
            "invalid just_typed channel data_type `{other}`"
        ))),
    }
}

fn parse_typed_channels_value(val: &Value) -> CoreResult<Vec<JustTypedChannelSpec>> {
    match val {
        Value::String(s) => {
            let mut out = Vec::new();
            for item in s.split(',') {
                let trimmed = item.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some((name, kind_str)) = trimmed.split_once(':') {
                    out.push(JustTypedChannelSpec::new(
                        name.trim(),
                        parse_just_typed_kind(kind_str)?,
                    ));
                } else {
                    out.push(JustTypedChannelSpec::new(
                        trimmed,
                        JustTypedChannelKind::Float,
                    ));
                }
            }
            Ok(out)
        }
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                if let Some(s) = item.as_str() {
                    out.push(JustTypedChannelSpec::new(
                        s.trim(),
                        JustTypedChannelKind::Float,
                    ));
                } else if let Some(obj) = item.as_object() {
                    let name = obj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();
                    let dt = obj
                        .get("data_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("float");
                    out.push(JustTypedChannelSpec::new(name, parse_just_typed_kind(dt)?));
                }
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

fn parse_channel_names_value(val: &Value) -> Vec<String> {
    match val {
        Value::String(s) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}
