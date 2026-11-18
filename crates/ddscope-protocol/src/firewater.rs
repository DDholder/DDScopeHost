//! SIMD-accelerated `FireWater` CSV text protocol parser and fast ASCII float parser (`FR-PR-02`).

use bytes::BytesMut;
use memchr::{memchr, memchr_iter};
use serde::{Deserialize, Serialize};

use crate::downlink::encode_downlink_default;
use crate::{DownlinkPayload, ProtocolCodec};
use ddscope_types::{
    ChannelDescriptor, CoreError, CoreResult, DecodedBatch, DecodedColumnBuf, ScalarType,
    StateEnumMap, StateLabel, TimestampNs,
};

const POW10_F64: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

const POW10_F32: [f32; 11] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10];

fn default_fw_source_id() -> String {
    "fire_water".to_string()
}

fn default_max_line_bytes() -> usize {
    64 * 1024
}

/// Default state label table matching the motor-control state machine convention.
pub fn default_sim_state_labels() -> StateEnumMap {
    let mut map = StateEnumMap::new();
    map.insert(
        0,
        StateLabel {
            name: "IDLE".to_string(),
            color_rgba: Some(0x6474_8BFF),
        },
    );
    map.insert(
        1,
        StateLabel {
            name: "ALIGN".to_string(),
            color_rgba: Some(0xF59E_0BFF),
        },
    );
    map.insert(
        2,
        StateLabel {
            name: "CLOSED_LOOP".to_string(),
            color_rgba: Some(0x10B9_81FF),
        },
    );
    map.insert(
        3,
        StateLabel {
            name: "OCP_FAULT".to_string(),
            color_rgba: Some(0xEF44_44FF),
        },
    );
    map
}

#[inline(always)]
fn trim_ascii_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&b) = s.first() {
        if b == b' ' || b == b'\t' {
            s = &s[1..];
        } else {
            break;
        }
    }
    while let Some(&b) = s.last() {
        if b == b' ' || b == b'\t' || b == b'\r' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// Fast zero-allocation ASCII `f64` parser.
///
/// Uses a branch-light `u64` mantissa + decimal point + exponent fast path that is IEEE-754 exact
/// for up to 15 significant digits and decimal exponents in `-22..=22`, falling back to
/// `core::str::FromStr` only on long/edge-case tokens. Rejects `NaN` and `Inf`.
#[inline]
pub fn parse_ascii_f64(raw: &[u8]) -> Option<f64> {
    let bytes = trim_ascii_ws(raw);
    let len = bytes.len();
    if len == 0 {
        return None;
    }
    if len > 32 {
        return parse_f64_fallback(bytes);
    }

    let mut i = 0usize;
    let neg = match bytes[0] {
        b'-' => {
            i = 1;
            true
        }
        b'+' => {
            i = 1;
            false
        }
        _ => false,
    };
    if i >= len {
        return None;
    }

    let mut mantissa: u64 = 0;
    let mut sig_digits: u8 = 0;
    let mut frac_digits: i32 = 0;
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut need_fallback = false;

    while i < len {
        let b = bytes[i];
        let d = b.wrapping_sub(b'0');
        if d <= 9 {
            seen_digit = true;
            if mantissa != 0 || d != 0 {
                sig_digits = sig_digits.saturating_add(1);
                if sig_digits > 15 {
                    need_fallback = true;
                    break;
                }
                mantissa = mantissa * 10 + u64::from(d);
            }
            if seen_dot {
                frac_digits += 1;
            }
            i += 1;
        } else if b == b'.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
            i += 1;
        } else if b == b'e' || b == b'E' {
            break;
        } else {
            return None;
        }
    }

    if need_fallback {
        return parse_f64_fallback(bytes);
    }
    if !seen_digit {
        return None;
    }

    let mut exp: i32 = 0;
    if i < len && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i >= len {
            return None;
        }
        let exp_neg = match bytes[i] {
            b'-' => {
                i += 1;
                true
            }
            b'+' => {
                i += 1;
                false
            }
            _ => false,
        };
        if i >= len {
            return None;
        }
        let mut exp_digits = 0u8;
        while i < len {
            let d = bytes[i].wrapping_sub(b'0');
            if d > 9 {
                return None;
            }
            exp_digits += 1;
            if exp_digits > 4 {
                return parse_f64_fallback(bytes);
            }
            exp = exp * 10 + i32::from(d);
            i += 1;
        }
        if exp_neg {
            exp = -exp;
        }
    }

    if mantissa == 0 {
        return Some(if neg { -0.0 } else { 0.0 });
    }

    let total_exp = exp - frac_digits;
    if !(-22..=22).contains(&total_exp) {
        return parse_f64_fallback(bytes);
    }

    let abs_val = if total_exp == 0 {
        mantissa as f64
    } else if total_exp > 0 {
        (mantissa as f64) * POW10_F64[total_exp as usize]
    } else {
        (mantissa as f64) / POW10_F64[(-total_exp) as usize]
    };

    if !abs_val.is_finite() {
        return None;
    }
    Some(if neg { -abs_val } else { abs_val })
}

#[cold]
fn parse_f64_fallback(bytes: &[u8]) -> Option<f64> {
    let s = core::str::from_utf8(bytes).ok()?;
    let v: f64 = s.parse().ok()?;
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// Fast zero-allocation ASCII `f32` parser.
///
/// Direct single-precision fast path for `<= 7` significant digits and decimal exponents in
/// `-10..=10`, falling back to `core::str::FromStr` for long/edge-case tokens. Rejects `NaN` and `Inf`.
#[inline]
pub fn parse_ascii_f32(raw: &[u8]) -> Option<f32> {
    let bytes = trim_ascii_ws(raw);
    let len = bytes.len();
    if len == 0 {
        return None;
    }
    if len > 24 {
        return parse_f32_fallback(bytes);
    }

    let mut i = 0usize;
    let neg = match bytes[0] {
        b'-' => {
            i = 1;
            true
        }
        b'+' => {
            i = 1;
            false
        }
        _ => false,
    };
    if i >= len {
        return None;
    }

    let mut mantissa: u32 = 0;
    let mut sig_digits: u8 = 0;
    let mut frac_digits: i32 = 0;
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut need_fallback = false;

    while i < len {
        let b = bytes[i];
        let d = b.wrapping_sub(b'0');
        if d <= 9 {
            seen_digit = true;
            if mantissa != 0 || d != 0 {
                sig_digits = sig_digits.saturating_add(1);
                if sig_digits > 7 {
                    need_fallback = true;
                    break;
                }
                mantissa = mantissa * 10 + u32::from(d);
            }
            if seen_dot {
                frac_digits += 1;
            }
            i += 1;
        } else if b == b'.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
            i += 1;
        } else if b == b'e' || b == b'E' {
            break;
        } else {
            return None;
        }
    }

    if need_fallback {
        return parse_f32_fallback(bytes);
    }
    if !seen_digit {
        return None;
    }

    let mut exp: i32 = 0;
    if i < len && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i >= len {
            return None;
        }
        let exp_neg = match bytes[i] {
            b'-' => {
                i += 1;
                true
            }
            b'+' => {
                i += 1;
                false
            }
            _ => false,
        };
        if i >= len {
            return None;
        }
        let mut exp_digits = 0u8;
        while i < len {
            let d = bytes[i].wrapping_sub(b'0');
            if d > 9 {
                return None;
            }
            exp_digits += 1;
            if exp_digits > 3 {
                return parse_f32_fallback(bytes);
            }
            exp = exp * 10 + i32::from(d);
            i += 1;
        }
        if exp_neg {
            exp = -exp;
        }
    }

    if mantissa == 0 {
        return Some(if neg { -0.0 } else { 0.0 });
    }

    let total_exp = exp - frac_digits;
    if !(-10..=10).contains(&total_exp) {
        return parse_f32_fallback(bytes);
    }

    let abs_val = if total_exp == 0 {
        mantissa as f32
    } else if total_exp > 0 {
        (mantissa as f32) * POW10_F32[total_exp as usize]
    } else {
        (mantissa as f32) / POW10_F32[(-total_exp) as usize]
    };

    if !abs_val.is_finite() {
        return None;
    }
    Some(if neg { -abs_val } else { abs_val })
}

#[cold]
fn parse_f32_fallback(bytes: &[u8]) -> Option<f32> {
    let s = core::str::from_utf8(bytes).ok()?;
    let v: f32 = s.parse().ok()?;
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

#[inline]
fn parse_ascii_bool(raw: &[u8]) -> Option<bool> {
    let b = trim_ascii_ws(raw);
    if b == b"0" || b.eq_ignore_ascii_case(b"false") {
        Some(false)
    } else if b == b"1" || b.eq_ignore_ascii_case(b"true") {
        Some(true)
    } else {
        None
    }
}

#[inline]
fn parse_ascii_i32(raw: &[u8]) -> Option<i32> {
    let bytes = trim_ascii_ws(raw);
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0usize;
    let neg = match bytes[0] {
        b'-' => {
            i = 1;
            true
        }
        b'+' => {
            i = 1;
            false
        }
        _ => false,
    };
    if i >= bytes.len() {
        return None;
    }
    let mut acc: i64 = 0;
    while i < bytes.len() {
        let d = bytes[i].wrapping_sub(b'0');
        if d > 9 {
            return None;
        }
        acc = acc.checked_mul(10)?.checked_add(i64::from(d))?;
        if acc > i64::from(i32::MAX) + 1 {
            return None;
        }
        i += 1;
    }
    let signed = if neg { -acc } else { acc };
    i32::try_from(signed).ok()
}

#[inline]
fn parse_state_token(raw: &[u8], custom_labels: &[(String, i32)]) -> Option<i32> {
    let tok = trim_ascii_ws(raw);
    if tok.is_empty() {
        return None;
    }
    for (name, code) in custom_labels {
        if tok.eq_ignore_ascii_case(name.as_bytes()) {
            return Some(*code);
        }
    }
    if let Some(code) = parse_ascii_i32(tok) {
        return Some(code);
    }
    if tok.eq_ignore_ascii_case(b"IDLE") {
        Some(0)
    } else if tok.eq_ignore_ascii_case(b"ALIGN") {
        Some(1)
    } else if tok.eq_ignore_ascii_case(b"RUN") || tok.eq_ignore_ascii_case(b"CLOSED_LOOP") {
        Some(2)
    } else if tok.eq_ignore_ascii_case(b"FAULT") || tok.eq_ignore_ascii_case(b"OCP_FAULT") {
        Some(3)
    } else {
        None
    }
}

/// Column data type supported by [`FireWaterCsvCodec`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FireWaterColumnKind {
    #[default]
    F32,
    F64,
    Bool,
    StateEnum {
        #[serde(default)]
        labels: StateEnumMap,
    },
}

/// Column specification for [`FireWaterCsvCodec`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireWaterColumnSpec {
    pub name: String,
    #[serde(default)]
    pub kind: FireWaterColumnKind,
    #[serde(default)]
    pub unit: Option<String>,
}

impl FireWaterColumnSpec {
    /// Creates an `F32` CSV column specification.
    pub fn f32(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: FireWaterColumnKind::F32,
            unit: None,
        }
    }

    /// Creates an `F64` CSV column specification.
    pub fn f64(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: FireWaterColumnKind::F64,
            unit: None,
        }
    }

    /// Creates a `Bool` logic CSV column specification.
    pub fn bool(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: FireWaterColumnKind::Bool,
            unit: None,
        }
    }

    /// Creates a `StateEnum` CSV column specification with optional custom state labels.
    pub fn state_enum(name: impl Into<String>, labels: StateEnumMap) -> Self {
        Self {
            name: name.into(),
            kind: FireWaterColumnKind::StateEnum { labels },
            unit: None,
        }
    }

    /// Sets unit metadata.
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }
}

/// Configuration for [`FireWaterCsvCodec`].
///
/// Accepts either simple `channel_names` (defaulting to `F32` columns, matching `ProtocolConfigDto`)
/// or typed `columns: Vec<FireWaterColumnSpec>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireWaterConfig {
    #[serde(default = "default_fw_source_id")]
    pub source_id: String,
    #[serde(default)]
    pub channel_names: Vec<String>,
    #[serde(default)]
    pub columns: Vec<FireWaterColumnSpec>,
    #[serde(default)]
    pub expected_tag: Option<String>,
    #[serde(default = "default_max_line_bytes")]
    pub max_line_bytes: usize,
}

impl FireWaterConfig {
    /// Creates a typed-column configuration.
    pub fn with_columns(
        source_id: impl Into<String>,
        columns: Vec<FireWaterColumnSpec>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            channel_names: Vec::new(),
            columns,
            expected_tag: None,
            max_line_bytes: default_max_line_bytes(),
        }
    }

    /// Creates an all-`F32` configuration from channel names.
    pub fn from_channel_names(source_id: impl Into<String>, channel_names: Vec<String>) -> Self {
        Self {
            source_id: source_id.into(),
            channel_names,
            columns: Vec::new(),
            expected_tag: None,
            max_line_bytes: default_max_line_bytes(),
        }
    }
}

/// Statistics tracked by [`FireWaterCsvCodec`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct FireWaterStats {
    pub decoded_rows: u64,
    pub malformed_lines: u64,
    pub tag_mismatch_lines: u64,
}

#[derive(Clone, Debug)]
enum CompiledColumn {
    F32,
    F64,
    Bool,
    StateEnum(Vec<(String, i32)>),
}

#[derive(Copy, Clone, Debug)]
enum ParsedCell {
    F32(f32),
    F64(f64),
    Bool(bool),
    State(i32),
}

/// SIMD newline-scanning `FireWater` CSV text protocol codec.
pub struct FireWaterCsvCodec {
    channels: Vec<ChannelDescriptor>,
    columns: Vec<CompiledColumn>,
    expected_tag: Option<Vec<u8>>,
    max_line_bytes: usize,
    carry: Vec<u8>,
    scratch_row: Vec<ParsedCell>,
    stats: FireWaterStats,
}

impl std::fmt::Debug for FireWaterCsvCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FireWaterCsvCodec")
            .field("columns", &self.columns.len())
            .field("carry_len", &self.carry.len())
            .field("stats", &self.stats)
            .finish()
    }
}

impl FireWaterCsvCodec {
    /// Constructs and validates a [`FireWaterCsvCodec`].
    pub fn new(config: FireWaterConfig) -> CoreResult<Self> {
        let specs: Vec<FireWaterColumnSpec> = if !config.columns.is_empty() {
            config.columns
        } else if !config.channel_names.is_empty() {
            config
                .channel_names
                .into_iter()
                .map(FireWaterColumnSpec::f32)
                .collect()
        } else {
            return Err(CoreError::InvalidConfig(
                "FireWaterCsvCodec requires at least 1 column".into(),
            ));
        };

        let mut channels = Vec::with_capacity(specs.len());
        let mut columns = Vec::with_capacity(specs.len());

        for spec in specs {
            if spec.name.is_empty() {
                return Err(CoreError::InvalidConfig(
                    "FireWaterCsvCodec column name must not be empty".into(),
                ));
            }
            match spec.kind {
                FireWaterColumnKind::F32 => {
                    let mut desc = ChannelDescriptor::new_scalar(
                        config.source_id.as_str(),
                        spec.name,
                        ScalarType::F32,
                    );
                    desc.unit = spec.unit;
                    channels.push(desc);
                    columns.push(CompiledColumn::F32);
                }
                FireWaterColumnKind::F64 => {
                    let mut desc = ChannelDescriptor::new_scalar(
                        config.source_id.as_str(),
                        spec.name,
                        ScalarType::F64,
                    );
                    desc.unit = spec.unit;
                    channels.push(desc);
                    columns.push(CompiledColumn::F64);
                }
                FireWaterColumnKind::Bool => {
                    let mut desc =
                        ChannelDescriptor::new_bool(config.source_id.as_str(), spec.name);
                    desc.unit = spec.unit;
                    channels.push(desc);
                    columns.push(CompiledColumn::Bool);
                }
                FireWaterColumnKind::StateEnum { labels } => {
                    let map = if labels.is_empty() {
                        default_sim_state_labels()
                    } else {
                        labels
                    };
                    let lookup: Vec<(String, i32)> = map
                        .iter()
                        .map(|(&code, lbl)| (lbl.name.clone(), code))
                        .collect();
                    let mut desc = ChannelDescriptor::new_state_machine(
                        config.source_id.as_str(),
                        spec.name,
                        map,
                    );
                    desc.unit = spec.unit;
                    channels.push(desc);
                    columns.push(CompiledColumn::StateEnum(lookup));
                }
            }
        }

        let scratch_row = vec![ParsedCell::F32(0.0); columns.len()];
        Ok(Self {
            channels,
            columns,
            expected_tag: config.expected_tag.map(|t| t.into_bytes()),
            max_line_bytes: config.max_line_bytes.max(64),
            carry: Vec::with_capacity(256),
            scratch_row,
            stats: FireWaterStats::default(),
        })
    }

    /// Returns cumulative decoded row and error counters.
    pub fn stats(&self) -> FireWaterStats {
        self.stats
    }

    fn ensure_batch_columns(&self, batch_out: &mut DecodedBatch) -> CoreResult<()> {
        if batch_out.columns.is_empty() {
            batch_out.columns = self
                .channels
                .iter()
                .map(|d| DecodedColumnBuf::for_channel_kind(&d.kind, 64))
                .collect();
            return Ok(());
        }
        if batch_out.columns.len() < self.columns.len() {
            return Err(CoreError::Protocol {
                message: format!(
                    "DecodedBatch column count {} is smaller than FireWater column count {}",
                    batch_out.columns.len(),
                    self.columns.len()
                ),
                recoverable: false,
            });
        }
        Ok(())
    }

    #[inline]
    fn parse_and_commit_line(&mut self, raw_line: &[u8], batch_out: &mut DecodedBatch) -> bool {
        let mut line = raw_line;
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        line = trim_ascii_ws(line);
        if line.is_empty() {
            return false;
        }

        // Check for optional `<tag>:` prefix before the first comma.
        let first_comma = line.iter().position(|&b| b == b',');
        let prefix_search_end = first_comma.unwrap_or(line.len());
        if let Some(colon_pos) = line[..prefix_search_end].iter().position(|&b| b == b':') {
            let tag = trim_ascii_ws(&line[..colon_pos]);
            if let Some(ref exp) = self.expected_tag {
                if tag != exp.as_slice() {
                    self.stats.tag_mismatch_lines += 1;
                    return false;
                }
            }
            line = &line[colon_pos + 1..];
        } else if self.expected_tag.is_some() {
            self.stats.tag_mismatch_lines += 1;
            return false;
        }

        let n_cols = self.columns.len();
        let mut rest = line;

        for idx in 0..n_cols {
            let tok = if idx + 1 == n_cols {
                // Last expected column: reject trailing extra commas.
                if rest.contains(&b',') {
                    self.stats.malformed_lines += 1;
                    return false;
                }
                rest
            } else {
                match rest.iter().position(|&b| b == b',') {
                    Some(cpos) => {
                        let t = &rest[..cpos];
                        rest = &rest[cpos + 1..];
                        t
                    }
                    None => {
                        self.stats.malformed_lines += 1;
                        return false;
                    }
                }
            };

            let parsed = match &self.columns[idx] {
                CompiledColumn::F32 => match parse_ascii_f32(tok) {
                    Some(v) => ParsedCell::F32(v),
                    None => {
                        self.stats.malformed_lines += 1;
                        return false;
                    }
                },
                CompiledColumn::F64 => match parse_ascii_f64(tok) {
                    Some(v) => ParsedCell::F64(v),
                    None => {
                        self.stats.malformed_lines += 1;
                        return false;
                    }
                },
                CompiledColumn::Bool => match parse_ascii_bool(tok) {
                    Some(v) => ParsedCell::Bool(v),
                    None => {
                        self.stats.malformed_lines += 1;
                        return false;
                    }
                },
                CompiledColumn::StateEnum(custom) => match parse_state_token(tok, custom) {
                    Some(code) => ParsedCell::State(code),
                    None => {
                        self.stats.malformed_lines += 1;
                        return false;
                    }
                },
            };
            self.scratch_row[idx] = parsed;
        }

        for (idx, cell) in self.scratch_row.iter().enumerate() {
            match (*cell, &mut batch_out.columns[idx]) {
                (ParsedCell::F32(v), DecodedColumnBuf::F32(col)) => col.push(v),
                (ParsedCell::F64(v), DecodedColumnBuf::F64(col)) => col.push(v),
                (ParsedCell::Bool(v), DecodedColumnBuf::Bool(col)) => col.push(v),
                (ParsedCell::State(v), DecodedColumnBuf::StateEnum(col)) => col.push(v),
                _ => {}
            }
        }
        self.stats.decoded_rows += 1;
        true
    }
}

impl ProtocolCodec for FireWaterCsvCodec {
    fn protocol_name(&self) -> &str {
        "FireWater"
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
        self.ensure_batch_columns(batch_out)?;
        batch_out.packet_recv_ns = recv_ns;
        if chunk.is_empty() {
            return Ok(0);
        }

        let mut rows_decoded = 0usize;
        let mut remaining = chunk;

        if !self.carry.is_empty() {
            match memchr(b'\n', remaining) {
                None => {
                    if self.carry.len() + remaining.len() > self.max_line_bytes {
                        self.carry.clear();
                        self.stats.malformed_lines += 1;
                    } else {
                        self.carry.extend_from_slice(remaining);
                    }
                    return Ok(0);
                }
                Some(nl_pos) => {
                    let mut line_buf = std::mem::take(&mut self.carry);
                    if line_buf.len() + nl_pos <= self.max_line_bytes {
                        line_buf.extend_from_slice(&remaining[..nl_pos]);
                        if self.parse_and_commit_line(&line_buf, batch_out) {
                            rows_decoded += 1;
                        }
                    } else {
                        self.stats.malformed_lines += 1;
                    }
                    line_buf.clear();
                    self.carry = line_buf;
                    remaining = &remaining[nl_pos + 1..];
                }
            }
        }

        let mut last_end = 0usize;
        for nl_pos in memchr_iter(b'\n', remaining) {
            let line = &remaining[last_end..nl_pos];
            last_end = nl_pos + 1;
            if self.parse_and_commit_line(line, batch_out) {
                rows_decoded += 1;
            }
        }

        if last_end < remaining.len() {
            let tail = &remaining[last_end..];
            if tail.len() > self.max_line_bytes {
                self.stats.malformed_lines += 1;
            } else {
                self.carry.extend_from_slice(tail);
            }
        }

        batch_out.sample_rows += rows_decoded;
        Ok(rows_decoded)
    }

    fn reset_state(&mut self) {
        self.carry.clear();
    }

    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        encode_downlink_default(req, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_float_parser_handles_standard_and_edge_cases() {
        let cases = [
            ("0", 0.0_f64),
            ("-0.0", -0.0),
            ("1.23", 1.23),
            ("-0.45", -0.45),
            ("+42.5", 42.5),
            (".25", 0.25),
            ("5.", 5.0),
            ("1.23e-4", 1.23e-4),
            ("-9.81E+2", -9.81e2),
            ("3.141592653589793", core::f64::consts::PI),
            ("0.0000000000000000000000001", 1e-25),
        ];
        for (txt, expected) in cases {
            let parsed = parse_ascii_f64(txt.as_bytes()).expect(txt);
            assert!(
                (parsed - expected).abs() <= 1e-15 * expected.abs().max(1.0),
                "mismatch for {txt}: got {parsed}, expected {expected}"
            );
            let parsed32 = parse_ascii_f32(txt.as_bytes()).expect(txt);
            assert!(
                (parsed32 - expected as f32).abs() <= 1e-6 * (expected as f32).abs().max(1.0)
            );
        }

        assert!(parse_ascii_f64(b"NaN").is_none());
        assert!(parse_ascii_f64(b"inf").is_none());
        assert!(parse_ascii_f64(b"1.2.3").is_none());
        assert!(parse_ascii_f64(b"").is_none());
        assert!(parse_ascii_f64(b".").is_none());
    }

    #[test]
    fn parses_tags_crlf_bools_states_and_recovers_from_malformed_lines() {
        let cfg = FireWaterConfig::with_columns(
            "uart1",
            vec![
                FireWaterColumnSpec::f32("ia"),
                FireWaterColumnSpec::f64("vbus"),
                FireWaterColumnSpec::bool("pwm"),
                FireWaterColumnSpec::state_enum("state", StateEnumMap::new()),
            ],
        );
        let mut codec = FireWaterCsvCodec::new(cfg).unwrap();
        let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 8);

        let stream = concat!(
            "motor: 1.23, 24.5, true, RUN\r\n",
            "bad_line_missing_cols, 1.0\n",
            "1.0, NaN, 1, IDLE\n",
            "motor: -0.45, 23.75, 0, OCP_FAULT\n",
            "0.25, 24.0, false, 1\r\n",
        );

        // Stream 1 byte at a time.
        for &b in stream.as_bytes() {
            codec.feed_bytes(&[b], 1234, &mut batch).unwrap();
        }
        assert_eq!(batch.sample_rows, 3);
        assert_eq!(codec.stats().malformed_lines, 2);
        assert_eq!(
            batch.columns[0].as_scalar_slice().unwrap().get_as_f64(1),
            Some(-0.45_f32 as f64)
        );
        assert_eq!(
            batch.columns[1].as_scalar_slice().unwrap().get_as_f64(0),
            Some(24.5)
        );
        assert_eq!(
            batch.columns[2].as_scalar_slice().unwrap().get_as_f64(0),
            Some(1.0)
        );
        assert_eq!(
            batch.columns[2].as_scalar_slice().unwrap().get_as_f64(1),
            Some(0.0)
        );
        assert_eq!(batch.columns[3].as_state_slice(), Some(&[2, 3, 1][..]));
    }
}
