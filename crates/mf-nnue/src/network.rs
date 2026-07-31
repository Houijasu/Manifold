// Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
// Eonego's copyright and MIT license notice are reproduced there.
// This port is part of Manifold and is distributed under GPL-3.0-only.

use std::fmt;
use std::path::Path;

use crate::format::{Cursor, FormatError};

pub const VERSION: u32 = 0x6A44_8AFA;
pub const HALF_KA_DIMS: usize = 22_528;
pub const THREAT_DIMS: usize = 60_720;
pub const L1: usize = 1_024;
pub const HALF: usize = 512;
pub const PSQT_BUCKETS: usize = 8;
pub const LAYER_STACKS: usize = 8;
pub const FC0_OUT: usize = 32;
pub const FC1_IN: usize = 64;
pub const FC1_OUT: usize = 32;
pub const FC2_IN: usize = 32;

#[repr(align(64))]
struct Aligned64<T>(T);

/// Row-major HalfKAv2_hm feature-transformer weights.
///
/// Every returned row begins at a 64-byte-aligned address.
pub struct HalfKaWeights {
    rows: Vec<Aligned64<[i16; L1]>>,
}

impl HalfKaWeights {
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[must_use]
    pub fn row(&self, feature: usize) -> Option<&[i16; L1]> {
        self.rows.get(feature).map(|row| &row.0)
    }
}

/// Row-major FullThreats feature-transformer weights.
///
/// Every returned row begins at a 64-byte-aligned address.
pub struct ThreatWeights {
    rows: Vec<Aligned64<[i8; L1]>>,
}

impl ThreatWeights {
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[must_use]
    pub fn row(&self, feature: usize) -> Option<&[i8; L1]> {
        self.rows.get(feature).map(|row| &row.0)
    }
}

/// One of the eight bucket-specific affine layer stacks.
pub struct LayerStack {
    fc0_biases: [i32; FC0_OUT],
    fc0_weights: Box<[i8]>,
    fc0_sparse_weights: Box<[i8]>,
    fc1_biases: [i32; FC1_OUT],
    fc1_weights: Box<[i8]>,
    fc2_bias: i32,
    fc2_weights: [i8; FC2_IN],
}

impl LayerStack {
    #[must_use]
    pub const fn fc0_biases(&self) -> &[i32; FC0_OUT] {
        &self.fc0_biases
    }

    #[must_use]
    pub fn fc0_weights(&self) -> &[i8] {
        &self.fc0_weights
    }

    /// Returns FC0 weights in 4-byte input-chunk-major, output-minor order.
    #[must_use]
    pub fn fc0_sparse_weights(&self) -> &[i8] {
        &self.fc0_sparse_weights
    }

    #[must_use]
    pub const fn fc1_biases(&self) -> &[i32; FC1_OUT] {
        &self.fc1_biases
    }

    #[must_use]
    pub fn fc1_weights(&self) -> &[i8] {
        &self.fc1_weights
    }

    #[must_use]
    pub const fn fc2_bias(&self) -> i32 {
        self.fc2_bias
    }

    #[must_use]
    pub const fn fc2_weights(&self) -> &[i8; FC2_IN] {
        &self.fc2_weights
    }
}

/// A parsed Eonego FullThreats NNUE network.
pub struct Network {
    version: u32,
    architecture_hash: u32,
    description: String,
    feature_transformer_hash: u32,
    feature_transformer_biases: [i16; L1],
    half_ka_weights: HalfKaWeights,
    threat_weights: ThreatWeights,
    psqt_weights: Box<[i32]>,
    threat_psqt_weights: Box<[i32]>,
    layer_stacks: [LayerStack; LAYER_STACKS],
}

impl Network {
    /// Parses a complete network and rejects any truncation or trailing bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LoadError> {
        let mut cursor = Cursor::new(bytes);
        let version = cursor.read_u32().map_err(LoadError::from)?;
        if version != VERSION {
            return Err(LoadError::UnexpectedVersion {
                found: version,
                expected: VERSION,
            });
        }

        let architecture_hash = cursor.read_u32().map_err(LoadError::from)?;
        let description_length = cursor.read_i32().map_err(LoadError::from)?;
        let description_length_usize = usize::try_from(description_length)
            .ok()
            .filter(|&length| length <= cursor.remaining())
            .ok_or(LoadError::InvalidDescriptionLength(description_length))?;
        let description = String::from_utf8_lossy(
            cursor
                .read_bytes(description_length_usize)
                .map_err(LoadError::from)?,
        )
        .into_owned();

        let feature_transformer_hash = cursor.read_u32().map_err(LoadError::from)?;

        let mut feature_transformer_biases = [0; L1];
        cursor
            .read_i16_into(&mut feature_transformer_biases)
            .map_err(LoadError::from)?;

        let threat_weights = read_threat_weights(&mut cursor)?;
        let threat_psqt_weights = read_i32_box(&mut cursor, THREAT_DIMS * PSQT_BUCKETS)?;
        let half_ka_weights = read_half_ka_weights(&mut cursor)?;
        let psqt_weights = read_i32_box(&mut cursor, HALF_KA_DIMS * PSQT_BUCKETS)?;

        let mut stacks = Vec::with_capacity(LAYER_STACKS);
        for _ in 0..LAYER_STACKS {
            stacks.push(read_layer_stack(&mut cursor)?);
        }
        let layer_stacks = stacks
            .try_into()
            .unwrap_or_else(|_| unreachable!("exact layer stack count"));

        if cursor.remaining() != 0 {
            return Err(LoadError::TrailingBytes(cursor.remaining()));
        }

        Ok(Self {
            version,
            architecture_hash,
            description,
            feature_transformer_hash,
            feature_transformer_biases,
            half_ka_weights,
            threat_weights,
            psqt_weights,
            threat_psqt_weights,
            layer_stacks,
        })
    }

    /// Reads and parses a complete network file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let bytes = std::fs::read(path).map_err(LoadError::Io)?;
        Self::from_bytes(&bytes)
    }

    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    #[must_use]
    pub const fn architecture_hash(&self) -> u32 {
        self.architecture_hash
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub const fn feature_transformer_hash(&self) -> u32 {
        self.feature_transformer_hash
    }

    #[must_use]
    pub const fn feature_transformer_biases(&self) -> &[i16; L1] {
        &self.feature_transformer_biases
    }

    #[must_use]
    pub const fn half_ka_weights(&self) -> &HalfKaWeights {
        &self.half_ka_weights
    }

    #[must_use]
    pub const fn threat_weights(&self) -> &ThreatWeights {
        &self.threat_weights
    }

    #[must_use]
    pub fn psqt_weights(&self) -> &[i32] {
        &self.psqt_weights
    }

    #[must_use]
    pub fn psqt_row(&self, feature: usize) -> Option<&[i32; PSQT_BUCKETS]> {
        row(&self.psqt_weights, feature)
    }

    #[must_use]
    pub fn threat_psqt_weights(&self) -> &[i32] {
        &self.threat_psqt_weights
    }

    #[must_use]
    pub fn threat_psqt_row(&self, feature: usize) -> Option<&[i32; PSQT_BUCKETS]> {
        row(&self.threat_psqt_weights, feature)
    }

    #[must_use]
    pub const fn layer_stacks(&self) -> &[LayerStack; LAYER_STACKS] {
        &self.layer_stacks
    }

    #[must_use]
    pub fn layer_stack(&self, bucket: usize) -> Option<&LayerStack> {
        self.layer_stacks.get(bucket)
    }
}

fn row<const N: usize>(values: &[i32], index: usize) -> Option<&[i32; N]> {
    let start = index.checked_mul(N)?;
    let end = start.checked_add(N)?;
    values.get(start..end)?.try_into().ok()
}

fn read_threat_weights(cursor: &mut Cursor<'_>) -> Result<ThreatWeights, LoadError> {
    let encoding = cursor.begin_array().map_err(LoadError::from)?;
    let mut rows = Vec::with_capacity(THREAT_DIMS);
    for _ in 0..THREAT_DIMS {
        let mut row = Aligned64([0; L1]);
        cursor
            .read_i8_into_with_encoding(&mut row.0, encoding)
            .map_err(LoadError::from)?;
        rows.push(row);
    }
    Ok(ThreatWeights { rows })
}

fn read_half_ka_weights(cursor: &mut Cursor<'_>) -> Result<HalfKaWeights, LoadError> {
    let encoding = cursor.begin_array().map_err(LoadError::from)?;
    let mut rows = Vec::with_capacity(HALF_KA_DIMS);
    for _ in 0..HALF_KA_DIMS {
        let mut row = Aligned64([0; L1]);
        cursor
            .read_i16_into_with_encoding(&mut row.0, encoding)
            .map_err(LoadError::from)?;
        rows.push(row);
    }
    Ok(HalfKaWeights { rows })
}

fn read_i32_box(cursor: &mut Cursor<'_>, length: usize) -> Result<Box<[i32]>, LoadError> {
    let mut values = vec![0; length].into_boxed_slice();
    cursor.read_i32_into(&mut values).map_err(LoadError::from)?;
    Ok(values)
}

fn read_i8_box(cursor: &mut Cursor<'_>, length: usize) -> Result<Box<[i8]>, LoadError> {
    let mut values = vec![0; length].into_boxed_slice();
    cursor.read_i8_into(&mut values).map_err(LoadError::from)?;
    Ok(values)
}

fn read_i32_array<const N: usize>(cursor: &mut Cursor<'_>) -> Result<[i32; N], LoadError> {
    let mut values = [0; N];
    cursor.read_i32_into(&mut values).map_err(LoadError::from)?;
    Ok(values)
}

fn read_i8_array<const N: usize>(cursor: &mut Cursor<'_>) -> Result<[i8; N], LoadError> {
    let mut values = [0; N];
    cursor.read_i8_into(&mut values).map_err(LoadError::from)?;
    Ok(values)
}

fn read_layer_stack(cursor: &mut Cursor<'_>) -> Result<LayerStack, LoadError> {
    cursor.read_u32().map_err(LoadError::from)?;
    let fc0_biases = read_i32_array(cursor)?;
    let fc0_weights = read_i8_box(cursor, FC0_OUT * L1)?;
    let fc0_sparse_weights = build_fc0_sparse_weights(&fc0_weights);
    let fc1_biases = read_i32_array(cursor)?;
    let fc1_weights = read_i8_box(cursor, FC1_OUT * FC1_IN)?;
    let [fc2_bias] = read_i32_array(cursor)?;
    let fc2_weights = read_i8_array(cursor)?;

    Ok(LayerStack {
        fc0_biases,
        fc0_weights,
        fc0_sparse_weights,
        fc1_biases,
        fc1_weights,
        fc2_bias,
        fc2_weights,
    })
}

pub(crate) fn build_fc0_sparse_weights(dense: &[i8]) -> Box<[i8]> {
    debug_assert_eq!(dense.len(), FC0_OUT * L1);
    let mut sparse = vec![0_i8; dense.len()].into_boxed_slice();
    for chunk in 0..L1 / 4 {
        for output in 0..FC0_OUT {
            let dense_offset = output * L1 + chunk * 4;
            let sparse_offset = (chunk * FC0_OUT + output) * 4;
            sparse[sparse_offset..sparse_offset + 4]
                .copy_from_slice(&dense[dense_offset..dense_offset + 4]);
        }
    }
    sparse
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    UnexpectedEof,
    UnexpectedVersion { found: u32, expected: u32 },
    InvalidDescriptionLength(i32),
    InvalidSignedLeb128,
    ValueOutOfRange { value: i64, target: &'static str },
    TrailingBytes(usize),
}

impl From<FormatError> for LoadError {
    fn from(error: FormatError) -> Self {
        match error {
            FormatError::UnexpectedEof => Self::UnexpectedEof,
            FormatError::InvalidSignedLeb128 => Self::InvalidSignedLeb128,
            FormatError::ValueOutOfRange { value, target } => {
                Self::ValueOutOfRange { value, target }
            }
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "could not read NNUE file: {error}"),
            Self::UnexpectedEof => formatter.write_str("unexpected end of NNUE data"),
            Self::UnexpectedVersion { found, expected } => write!(
                formatter,
                "unexpected NNUE version 0x{found:08X} (expected 0x{expected:08X})"
            ),
            Self::InvalidDescriptionLength(length) => {
                write!(formatter, "invalid NNUE description length {length}")
            }
            Self::InvalidSignedLeb128 => formatter.write_str("invalid signed LEB128 value"),
            Self::ValueOutOfRange { value, target } => {
                write!(formatter, "value {value} is out of range for {target}")
            }
            Self::TrailingBytes(length) => {
                write!(formatter, "{length} trailing bytes after NNUE network")
            }
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FC0_OUT, L1, LoadError, Network, VERSION, build_fc0_sparse_weights, row};

    #[test]
    fn row_returns_none_when_range_arithmetic_overflows() {
        assert!(row::<2>(&[], usize::MAX).is_none());
        assert!(row::<8>(&[], usize::MAX / 8).is_none());
    }

    #[test]
    fn row_returns_only_complete_in_bounds_rows() {
        let values = [10, 11, 20, 21];
        assert_eq!(row::<2>(&values, 0), Some(&[10, 11]));
        assert_eq!(row::<2>(&values, 1), Some(&[20, 21]));
        assert_eq!(row::<2>(&values, 2), None);
    }

    #[test]
    fn rejects_wrong_version() {
        let bytes = 0x1234_5678_u32.to_le_bytes();
        assert!(matches!(
            Network::from_bytes(&bytes),
            Err(LoadError::UnexpectedVersion {
                found: 0x1234_5678,
                expected: VERSION
            })
        ));
    }

    #[test]
    fn rejects_negative_description_length() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&(-1_i32).to_le_bytes());

        assert!(matches!(
            Network::from_bytes(&bytes),
            Err(LoadError::InvalidDescriptionLength(-1))
        ));
    }

    #[test]
    fn rejects_description_length_larger_than_remaining_input() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&5_i32.to_le_bytes());
        bytes.extend_from_slice(b"tiny");

        assert!(matches!(
            Network::from_bytes(&bytes),
            Err(LoadError::InvalidDescriptionLength(5))
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(matches!(
            Network::from_bytes(&VERSION.to_le_bytes()),
            Err(LoadError::UnexpectedEof)
        ));
    }

    #[test]
    fn sparse_fc0_layout_is_chunk_major_with_all_outputs_contiguous() {
        let dense = (0..FC0_OUT * L1)
            .map(|index| index.wrapping_mul(17) as i8)
            .collect::<Vec<_>>();
        let sparse = build_fc0_sparse_weights(&dense);

        assert_eq!(sparse.len(), dense.len());
        for chunk in [0, 1, 127, 255] {
            for output in [0, 1, 15, 31] {
                let dense_offset = output * L1 + chunk * 4;
                let sparse_offset = (chunk * FC0_OUT + output) * 4;
                assert_eq!(
                    &sparse[sparse_offset..sparse_offset + 4],
                    &dense[dense_offset..dense_offset + 4],
                    "chunk {chunk}, output {output}"
                );
            }
        }
    }
}
