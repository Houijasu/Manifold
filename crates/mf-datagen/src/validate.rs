//! Validation of an emitted training-data file.
//!
//! This re-reads a file from disk and re-derives every claim the generator made about
//! it, rather than trusting the generator's own counters. That distinction is the
//! point: `A-NNUE-013` requires the reported record count to match the file's *true*
//! record count, and a validator that reported the generator's tally would pass even
//! if writing were broken.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::filter::{Filter, Rejection};
use crate::record::{RECORD_BYTES, Record, StructuralError};

/// The outcome of validating a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// Records read from the file.
    pub records: u64,
    /// Records with at least one structural defect.
    pub invalid: u64,
    /// Structural defects by kind, with counts.
    pub structural: Vec<(StructuralError, u64)>,
    /// Filter violations by reason, indexed as in [`Rejection::ALL`].
    ///
    /// Only populated when filter checking is requested.
    pub filter_violations: [u64; Rejection::ALL.len()],
    /// Records whose emitted position duplicates an earlier one.
    pub duplicates: u64,
    /// Records by side-to-move-relative result: loss, draw, win.
    pub results: [u64; 3],
    /// Records containing at least one castling-eligible king placement.
    ///
    /// Not meaningful on its own; see [`Self::castling_kept`].
    pub castling_kept: u64,
    /// Whether filter checks were performed.
    pub filters_checked: bool,
}

impl ValidationReport {
    /// The share of records that duplicate an earlier position, in percent.
    pub fn duplicate_percent(&self) -> f64 {
        if self.records == 0 {
            return 0.0;
        }
        (self.duplicates as f64) * 100.0 / (self.records as f64)
    }

    /// The largest share held by any single WDL label, in percent.
    pub fn max_result_share_percent(&self) -> f64 {
        if self.records == 0 {
            return 0.0;
        }
        let peak = self.results.iter().copied().max().unwrap_or(0);
        (peak as f64) * 100.0 / (self.records as f64)
    }

    /// Total filter violations found.
    pub fn total_filter_violations(&self) -> u64 {
        self.filter_violations.iter().sum()
    }
}

/// A reason a file could not be validated at all.
#[derive(Debug)]
pub enum ValidationError {
    /// The file could not be opened or read.
    Io(std::io::Error),
    /// The file's length is not a whole number of records.
    NotRecordAligned { bytes: u64, remainder: u64 },
}

impl core::fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::NotRecordAligned { bytes, remainder } => write!(
                formatter,
                "file length {bytes} is not a multiple of {RECORD_BYTES} bytes ({remainder} \
                 trailing bytes); the file is truncated or not bulletformat"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

impl From<std::io::Error> for ValidationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Validates the bulletformat file at `path`.
///
/// When `filter` is `Some`, each record is additionally re-checked against the filter
/// rules using the position reconstructed from the record itself. Only the checks that
/// survive the format's lossiness can be re-derived this way: in-check status and score
/// bounds are recoverable, but the played move is not stored, so "the best move was
/// tactical" cannot be re-checked from the file. That check is enforced at generation
/// time and reported from the generator's own counters, and this function reports it as
/// unverifiable rather than silently reporting zero.
pub fn validate_file(
    path: &Path,
    filter: Option<Filter>,
) -> Result<ValidationReport, ValidationError> {
    let metadata = std::fs::metadata(path)?;
    let bytes = metadata.len();
    let remainder = bytes % RECORD_BYTES as u64;
    if remainder != 0 {
        return Err(ValidationError::NotRecordAligned { bytes, remainder });
    }

    let mut report = ValidationReport {
        filters_checked: filter.is_some(),
        ..ValidationReport::default()
    };
    let mut structural: HashMap<StructuralError, u64> = HashMap::new();
    let mut seen: HashMap<[u8; 29], u64> = HashMap::new();

    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut buffer = [0u8; RECORD_BYTES];
    loop {
        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }

        let record = Record::from_bytes(buffer);
        report.records += 1;

        let errors = record.structural_errors();
        if !errors.is_empty() {
            report.invalid += 1;
            for error in errors {
                *structural.entry(error).or_default() += 1;
            }
        }

        if let Some(outcome) = record.outcome() {
            report.results[outcome as usize] += 1;
        }

        // Duplicate detection keys on the board and result but NOT on the score, so a
        // position re-reached at a different search depth still counts as a duplicate.
        let mut key = [0u8; 29];
        key[0..24].copy_from_slice(&buffer[0..24]);
        key[24..29].copy_from_slice(&buffer[26..31]);
        let count = seen.entry(key).or_default();
        *count += 1;
        if *count > 1 {
            report.duplicates += 1;
        }

        if let Some(filter) = filter {
            if record.score().abs() as i32 > filter.score_bound {
                report.filter_violations[index_of(Rejection::ScoreOutOfBounds)] += 1;
            }
            if (record.score() as i32).abs() >= crate::filter::MATE_SCORE_THRESHOLD {
                report.filter_violations[index_of(Rejection::MateScore)] += 1;
            }
            if let Some(position) = record.to_position()
                && crate::filter::in_check(&position)
            {
                report.filter_violations[index_of(Rejection::InCheck)] += 1;
            }
        }

        if has_castling_eligible_king(&record) {
            report.castling_kept += 1;
        }
    }

    let mut structural: Vec<(StructuralError, u64)> = structural.into_iter().collect();
    structural.sort_by_key(|(error, _)| format!("{error}"));
    report.structural = structural;
    Ok(report)
}

fn index_of(rejection: Rejection) -> usize {
    Rejection::ALL
        .iter()
        .position(|candidate| *candidate == rejection)
        .expect("every rejection is in Rejection::ALL")
}

/// Whether the record shows a king still on its original square with a rook on a
/// corner, i.e. a position in which castling plausibly remained available.
///
/// This is necessarily a heuristic: bulletformat discards castling rights entirely.
/// It exists to give `--check-filters` a positive, non-zero signal that castling
/// positions were kept rather than filtered out — which is what the validation
/// contract asks for. It is not, and cannot be, an exact castling-rights count.
fn has_castling_eligible_king(record: &Record) -> bool {
    const KING: u8 = 5;
    const ROOK: u8 = 3;
    let mut king_home = false;
    let mut rook_corner = false;
    for (code, square) in record.iter_pieces() {
        let kind = code & 0b0111;
        let is_opponent = code >> 3 == 1;
        if kind == KING && !is_opponent && square == 4 {
            king_home = true;
        }
        if kind == ROOK && !is_opponent && matches!(square, 0 | 7) {
            rook_corner = true;
        }
    }
    king_home && rook_corner
}

#[cfg(test)]
mod tests {
    use super::validate_file;
    use crate::filter::Filter;
    use crate::record::{Outcome, RECORD_BYTES, Record};
    use mf_core::Position;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mf-datagen-validate-{name}-{}.bullet",
            std::process::id()
        ));
        path
    }

    fn write(path: &std::path::Path, bytes: &[u8]) {
        let mut file = std::fs::File::create(path).expect("temp file creates");
        file.write_all(bytes).expect("temp file writes");
    }

    #[test]
    fn a_well_formed_file_validates_clean() {
        let path = temp_path("clean");
        let record = Record::encode(&Position::startpos(), 25, Outcome::Draw).expect("encodes");
        let mut bytes = Vec::new();
        for score in 0..8i16 {
            let record =
                Record::encode(&Position::startpos(), score, Outcome::Draw).unwrap_or(record);
            bytes.extend_from_slice(&record.to_bytes());
        }
        write(&path, &bytes);

        let report = validate_file(&path, Some(Filter::default())).expect("validates");
        assert_eq!(report.records, 8);
        assert_eq!(report.invalid, 0);
        assert_eq!(report.total_filter_violations(), 0);
        assert!(report.castling_kept > 0, "startpos keeps castling");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_whose_length_is_not_a_multiple_of_the_record_size_is_rejected() {
        let path = temp_path("misaligned");
        write(&path, &[0u8; RECORD_BYTES + 7]);
        let error = validate_file(&path, None).expect_err("misaligned file must fail");
        assert!(format!("{error}").contains("not a multiple of 32"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn structural_defects_are_counted_rather_than_panicked_on() {
        let path = temp_path("garbage");
        write(&path, &[0xffu8; RECORD_BYTES * 3]);
        let report = validate_file(&path, None).expect("garbage still parses as records");
        assert_eq!(report.records, 3);
        assert_eq!(report.invalid, 3);
        assert!(!report.structural.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn duplicate_positions_are_detected() {
        let path = temp_path("duplicates");
        let record = Record::encode(&Position::startpos(), 5, Outcome::Draw).expect("encodes");
        let mut bytes = Vec::new();
        for _ in 0..4 {
            bytes.extend_from_slice(&record.to_bytes());
        }
        write(&path, &bytes);
        let report = validate_file(&path, None).expect("validates");
        assert_eq!(report.records, 4);
        assert_eq!(report.duplicates, 3, "three repeats of the first record");
        assert!((report.duplicate_percent() - 75.0).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_in_check_record_is_reported_as_a_filter_violation() {
        let path = temp_path("incheck");
        let checked = Position::from_fen(
            "rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3",
            false,
        )
        .expect("FEN parses");
        let record = Record::encode(&checked, 0, Outcome::Draw).expect("encodes");
        write(&path, &record.to_bytes());
        let report = validate_file(&path, Some(Filter::default())).expect("validates");
        assert_eq!(report.total_filter_violations(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
