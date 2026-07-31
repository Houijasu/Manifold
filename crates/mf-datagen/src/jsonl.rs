//! Conversion of the Lichess evaluation database into bulletformat records.
//!
//! The bootstrap corpus is **downloaded, not self-played**. `M5-F1` measured self-play
//! datagen at ~836 positions/second, which puts a 100M-position corpus at ~33 hours and
//! the ~1B-position target at roughly two weeks. `https://database.lichess.org/lichess_db_eval.jsonl.zst`
//! is **CC0**, ~394.7M positions already evaluated by Stockfish, and fetches in about
//! 45 minutes at the measured ~8 MB/s link ceiling. The pre-converted HuggingFace
//! binpacks are faster still but are Leela-derived and therefore ODbL share-alike, so
//! they are ruled out by the mission's licensing posture.
//!
//! Three properties of the source format drive everything here.
//!
//! * **There is no game result.** These are *analysed* positions, not played games, so
//!   there is no win/draw/loss outcome — but bulletformat's 32-byte record requires a
//!   result byte. Sigmoiding the eval into a fake label and then also training against
//!   it would be circular: the network would be taught nothing beyond the eval it is
//!   already being fitted to. Every record therefore carries the neutral placeholder
//!   [`Outcome::Draw`], and rung 1 trains with bullet's **WDL lambda at pure eval
//!   (`lambda = 0.0`)** so the result byte contributes nothing to the loss. Genuine WDL
//!   labels arrive later from self-play, which is what `M5-F1`'s generator is for.
//! * **Scores are WHITE-relative**, where bulletformat is side-to-move relative.
//!   Determined empirically rather than assumed, by comparing 30 decisive
//!   black-to-move positions against Stockfish (whose UCI `score cp` is
//!   side-to-move relative by definition): white-relative agreed on the sign 29/30,
//!   side-to-move relative 0/30. Only black-to-move positions discriminate.
//! * **One position carries several evals at different depths.** [`pick_eval`] takes
//!   the deepest, breaking ties on `knodes` and then on file order.
//!
//! The parser is a hand-rolled byte scanner rather than a JSON crate because
//! `mf-datagen` has **zero runtime dependencies** by design (see [`crate::record`]),
//! and because only four fields per line are wanted out of a record whose principal
//! weight is principal-variation text that is read one move deep and then discarded.

use std::io::{BufRead, Write};

use mf_core::{Color, Move, Position, parse_uci_move};

use crate::filter::{Filter, Rejection};
use crate::record::{Outcome, RECORD_BYTES, Record};

/// The centipawn magnitude a mate announcement saturates to.
///
/// A mate score is not a centipawn quantity, so it cannot be used as a regression
/// target directly. Dropping mates instead would bias the corpus away from decisive
/// positions, and scaling by distance-to-mate would invent a centipawn scale that the
/// source does not have. Saturating at a single large magnitude says exactly what is
/// known — "this side is winning by more than the eval scale can express" — and the
/// count of affected records is reported so the size of that lump is never hidden.
///
/// This equals [`crate::DEFAULT_SCORE_BOUND`], and is additionally clamped to the
/// configured bound at conversion time so that lowering `--score-bound` cannot make
/// every mate fall out as out-of-bounds.
pub const MATE_SATURATION_CP: i32 = crate::DEFAULT_SCORE_BOUND;

/// How the deepest eval is chosen when a position carries several.
///
/// Reported verbatim in the conversion summary so the rule is recorded with the data
/// rather than only in this source file.
pub const TIE_BREAK_RULE: &str = "max-depth, then max-knodes, then first-in-file";

/// The WDL lambda rung 1 trains with.
///
/// `0.0` is pure eval: the result byte is not part of the loss. See the module docs.
pub const RUNG1_WDL_LAMBDA: f32 = 0.0;

/// A score exactly as the source states it, before any conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawScore {
    /// Centipawns, white-relative.
    Cp(i32),
    /// Moves to mate, white-relative: positive means white mates.
    Mate(i32),
}

/// The eval chosen for one position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PickedEval<'a> {
    pub depth: u32,
    pub knodes: u64,
    pub score: RawScore,
    /// The first move of the best principal variation, in UCI.
    pub first_move: &'a str,
}

/// One parsed source line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedLine<'a> {
    /// The source FEN, which carries only four fields (no clocks).
    pub fen: &'a str,
    pub eval: PickedEval<'a>,
}

/// Why a source line produced no record.
///
/// These are conversion-side reasons, distinct from [`Rejection`], which covers the
/// position filters shared with self-play generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The line is not a JSON object with the fields this converter needs.
    Unparseable,
    /// No eval on the line carried a usable score and principal variation.
    NoEval,
    /// The FEN did not parse, or violated a `Position` invariant.
    BadFen,
    /// The principal variation's first move is not legal in the stated position.
    BadPvMove,
    /// The record could not be encoded (missing king, impossible piece count).
    Unencodable,
}

impl SkipReason {
    /// Every variant, in report order.
    pub const ALL: [Self; 5] = [
        Self::Unparseable,
        Self::NoEval,
        Self::BadFen,
        Self::BadPvMove,
        Self::Unencodable,
    ];

    /// The stable `key=value` identifier used in conversion reports.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unparseable => "unparseable",
            Self::NoEval => "no_eval",
            Self::BadFen => "bad_fen",
            Self::BadPvMove => "bad_pv_move",
            Self::Unencodable => "unencodable",
        }
    }
}

/// Conversion settings.
#[derive(Clone, Copy, Debug)]
pub struct ConvertConfig {
    /// The position filters, shared with self-play generation.
    pub filter: Filter,
    /// The magnitude a mate announcement saturates to, before clamping to the bound.
    pub mate_saturation_cp: i32,
    /// Stop after emitting this many records, if set.
    pub max_positions: Option<u64>,
    /// Source lines to discard before converting, used to restart mid-file.
    pub skip_lines: u64,
}

impl Default for ConvertConfig {
    fn default() -> Self {
        Self {
            filter: Filter::default(),
            mate_saturation_cp: MATE_SATURATION_CP,
            max_positions: None,
            skip_lines: 0,
        }
    }
}

impl ConvertConfig {
    /// The magnitude a mate actually saturates to, after clamping to the score bound.
    ///
    /// Clamped so that a caller who lowers `--score-bound` below the saturation value
    /// does not silently convert every mate into an out-of-bounds rejection.
    pub fn effective_mate_cp(&self) -> i32 {
        self.mate_saturation_cp.min(self.filter.score_bound)
    }
}

/// What a conversion run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConvertStats {
    /// Source lines read, excluding those skipped by [`ConvertConfig::skip_lines`].
    pub lines: u64,
    /// Lines that parsed and yielded a position to filter.
    pub considered: u64,
    /// Records written.
    pub positions: u64,
    /// Filter rejections, indexed as in [`Rejection::ALL`].
    pub rejected: [u64; Rejection::ALL.len()],
    /// Conversion skips, indexed as in [`SkipReason::ALL`].
    pub skipped: [u64; SkipReason::ALL.len()],
    /// Records whose score came from a mate announcement and was saturated.
    pub mate_converted: u64,
    /// The total number of source lines consumed, including skipped ones.
    ///
    /// This is the value a restart resumes from.
    pub lines_consumed: u64,
}

impl ConvertStats {
    /// Total lines that produced no record for a conversion-side reason.
    pub fn total_skipped(&self) -> u64 {
        self.skipped.iter().sum()
    }

    /// Total lines that produced no record because a filter rejected the position.
    pub fn total_rejected(&self) -> u64 {
        self.rejected.iter().sum()
    }

    fn skip(&mut self, reason: SkipReason) {
        self.skipped[reason as usize] += 1;
    }
}

/// Converts a JSONL stream into bulletformat records.
///
/// `sink` receives each encoded record; it is called once per emitted position rather
/// than being handed the whole corpus, because the corpus does not fit in memory.
/// `progress` is invoked periodically with the running stats so a caller can persist a
/// restart point without this module owning a file format.
pub fn convert<R, S, P>(
    mut reader: R,
    config: ConvertConfig,
    mut sink: S,
    mut progress: P,
) -> Result<ConvertStats, String>
where
    R: BufRead,
    S: FnMut(&[u8; RECORD_BYTES]) -> Result<(), String>,
    P: FnMut(&ConvertStats) -> Result<(), String>,
{
    const PROGRESS_INTERVAL: u64 = 1_000_000;

    let mut stats = ConvertStats::default();
    let mut buffer = Vec::with_capacity(8192);
    let mut next_progress = PROGRESS_INTERVAL;

    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|error| format!("unable to read source line: {error}"))?;
        if read == 0 {
            break;
        }
        stats.lines_consumed += 1;
        if stats.lines_consumed <= config.skip_lines {
            continue;
        }
        stats.lines += 1;

        convert_line(&buffer, config, &mut stats, &mut sink)?;

        if let Some(limit) = config.max_positions
            && stats.positions >= limit
        {
            break;
        }
        if stats.lines >= next_progress {
            next_progress += PROGRESS_INTERVAL;
            progress(&stats)?;
        }
    }

    progress(&stats)?;
    Ok(stats)
}

fn convert_line<S>(
    line: &[u8],
    config: ConvertConfig,
    stats: &mut ConvertStats,
    sink: &mut S,
) -> Result<(), String>
where
    S: FnMut(&[u8; RECORD_BYTES]) -> Result<(), String>,
{
    let Ok(line) = core::str::from_utf8(line) else {
        stats.skip(SkipReason::Unparseable);
        return Ok(());
    };
    let line = line.trim_end();
    if line.is_empty() {
        stats.skip(SkipReason::Unparseable);
        return Ok(());
    }
    let Some(parsed) = parse_line(line) else {
        // A line that is syntactically fine but carries no scored PV is a different
        // fact from a malformed line, and is worth separating in the report.
        if line.contains("\"fen\"") {
            stats.skip(SkipReason::NoEval);
        } else {
            stats.skip(SkipReason::Unparseable);
        }
        return Ok(());
    };

    let Some(position) = parse_source_fen(parsed.fen) else {
        stats.skip(SkipReason::BadFen);
        return Ok(());
    };
    let Some(best_move) = parse_pv_move(&position, parsed.eval.first_move) else {
        stats.skip(SkipReason::BadPvMove);
        return Ok(());
    };

    let (score, from_mate) = to_side_to_move_centipawns(
        parsed.eval.score,
        position.side_to_move(),
        config.effective_mate_cp(),
    );

    stats.considered += 1;
    if let Some(rejection) = config.filter.rejection(&position, Some(best_move), score) {
        stats.rejected[rejection as usize] += 1;
        return Ok(());
    }

    // The source has no game result, so every record carries the same neutral
    // placeholder and rung 1 trains at WDL lambda 0.0. See the module docs.
    let record = match Record::encode(&position, score as i16, Outcome::Draw) {
        Ok(record) => record,
        Err(_) => {
            stats.skip(SkipReason::Unencodable);
            return Ok(());
        }
    };
    sink(&record.to_bytes())?;
    stats.positions += 1;
    if from_mate {
        stats.mate_converted += 1;
    }
    Ok(())
}

/// Maps a white-relative source score onto a side-to-move-relative centipawn value.
///
/// Returns the score and whether it came from a mate announcement.
pub fn to_side_to_move_centipawns(
    score: RawScore,
    side_to_move: Color,
    mate_cp: i32,
) -> (i32, bool) {
    let (white_relative, from_mate) = match score {
        RawScore::Cp(cp) => (cp, false),
        RawScore::Mate(moves) => {
            // `mate: 0` is not a distance any real announcement carries; treating it as
            // a win for white rather than as 0 cp keeps the sign total.
            let sign = if moves < 0 { -1 } else { 1 };
            (sign * mate_cp, true)
        }
    };
    let relative = match side_to_move {
        Color::White => white_relative,
        Color::Black => -white_relative,
    };
    (relative, from_mate)
}

/// Resolves a principal-variation move against the position, in either castling
/// notation.
///
/// The source writes castling as **king-takes-rook** (`e1h1`, `e8a8`) even for
/// standard chess, which is the Chess960 convention. Parsing it as standard-only
/// rejects the move, and the position is then dropped — which would silently strip the
/// corpus of exactly the king-safety positions the network most needs, and would do so
/// without any signal beyond a skip counter nobody would question. Measured on a
/// 28,633-line sample: **638 lines, all of them castling**, verified independently with
/// `python-chess` (every one was a king move onto its own rook).
///
/// Standard notation is tried first because it is the overwhelming majority of moves
/// and the two forms cannot collide: `e1g1` is a king move to an empty or
/// enemy-occupied square, `e1h1` is a king move onto its own rook.
fn parse_pv_move(position: &Position, notation: &str) -> Option<Move> {
    parse_uci_move(position, notation, false).or_else(|| parse_uci_move(position, notation, true))
}

/// Parses the source's four-field FEN.
///
/// `lichess_db_eval` omits the halfmove clock and fullmove number, which `Position`
/// requires, so the canonical `0 1` is appended. Neither field is encoded in a training
/// record, so the substitution is lossless as far as the corpus is concerned.
pub fn parse_source_fen(fen: &str) -> Option<Position> {
    let fields = fen.split_whitespace().count();
    let owned;
    let full = match fields {
        4 => {
            owned = format!("{fen} 0 1");
            owned.as_str()
        }
        6 => fen,
        _ => return None,
    };
    Position::from_fen(full, false).ok()
}

/// Extracts the FEN and the deepest usable eval from one source line.
///
/// Returns `None` when the line carries no `fen`, or when no eval on it has both a
/// score and a principal variation.
pub fn parse_line(line: &str) -> Option<ParsedLine<'_>> {
    let fen_start = find_after(line, "\"fen\":\"")?;
    let (fen, _) = read_until_quote(line, fen_start)?;
    let evals_start = find_after(line, "\"evals\":[")?;
    let eval = pick_eval(&line[evals_start..])?;
    Some(ParsedLine { fen, eval })
}

/// Chooses the deepest eval from the body of an `evals` array.
///
/// `evals_body` starts immediately after the array's opening bracket.
///
/// The tie-break is [`TIE_BREAK_RULE`]: greatest `depth` wins; equal depths are broken
/// by greater `knodes`, because at the same depth more nodes means a wider or less
/// aggressively pruned search; a full tie keeps the earlier entry, so the choice is a
/// pure function of the line and the corpus is reproducible.
pub fn pick_eval(evals_body: &str) -> Option<PickedEval<'_>> {
    let bytes = evals_body.as_bytes();
    let mut index = 0usize;
    let mut best: Option<PickedEval<'_>> = None;

    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                let (object, after) = object_slice(evals_body, index)?;
                if let Some(candidate) = parse_eval_object(object)
                    && best.is_none_or(|current| {
                        (candidate.depth, candidate.knodes) > (current.depth, current.knodes)
                    })
                {
                    best = Some(candidate);
                }
                index = after;
            }
            b']' => break,
            _ => index += 1,
        }
    }
    best
}

fn parse_eval_object(object: &str) -> Option<PickedEval<'_>> {
    let depth = find_after(object, "\"depth\":").and_then(|at| read_int(object, at))?;
    let knodes = find_after(object, "\"knodes\":")
        .and_then(|at| read_int(object, at))
        .unwrap_or(0);
    let pvs_start = find_after(object, "\"pvs\":[")?;
    // The source lists principal variations best-first, so the first entry is the one
    // whose score is the position's evaluation.
    let first_pv_start = object[pvs_start..].find('{')? + pvs_start;
    let (pv, _) = object_slice(object, first_pv_start)?;

    let score = match find_after(pv, "\"cp\":") {
        Some(at) => RawScore::Cp(read_int(pv, at)? as i32),
        None => RawScore::Mate(find_after(pv, "\"mate\":").and_then(|at| read_int(pv, at))? as i32),
    };
    let line_start = find_after(pv, "\"line\":\"")?;
    let (line, _) = read_until_quote(pv, line_start)?;
    let first_move = line.split(' ').next().filter(|mv| !mv.is_empty())?;

    Some(PickedEval {
        depth: u32::try_from(depth).ok()?,
        knodes: u64::try_from(knodes).unwrap_or(0),
        score,
        first_move,
    })
}

/// The index just past `needle` in `haystack`, if present.
fn find_after(haystack: &str, needle: &str) -> Option<usize> {
    haystack.find(needle).map(|at| at + needle.len())
}

/// Reads a string body starting at `at`, up to the next unescaped `"`.
///
/// The fields read here — FENs and UCI principal variations — contain no escapes, so
/// an escaped quote is treated as malformed rather than decoded.
fn read_until_quote(text: &str, at: usize) -> Option<(&str, usize)> {
    let end = text[at..].find('"')? + at;
    Some((&text[at..end], end + 1))
}

/// Reads a signed integer starting at `at`.
fn read_int(text: &str, at: usize) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut index = at;
    let negative = bytes.get(index) == Some(&b'-');
    if negative {
        index += 1;
    }
    let start = index;
    while matches!(bytes.get(index), Some(byte) if byte.is_ascii_digit()) {
        index += 1;
    }
    if index == start {
        return None;
    }
    let value: i64 = text[start..index].parse().ok()?;
    Some(if negative { -value } else { value })
}

/// Slices the JSON object beginning at `start`, returning it and the index after it.
///
/// Brace matching is string-aware: a `{` inside a quoted value must not open a level.
/// The fields this converter reads never contain braces, but a scanner that assumes so
/// fails silently and catastrophically on the one line that does.
fn object_slice(text: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut index = start;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((&text[start..=index], index + 1));
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

/// Writes a restart point: the source lines consumed and the records written.
///
/// A 21.4 GB download must never be repeated after a crash, and neither should hours of
/// conversion. The pair is written to a sidecar so a restart can truncate the output to
/// a record boundary it knows is complete and skip exactly the input it already read.
pub fn write_progress<W: Write>(mut writer: W, stats: &ConvertStats) -> Result<(), String> {
    writeln!(
        writer,
        "lines_consumed={}\nrecords={}",
        stats.lines_consumed, stats.positions
    )
    .map_err(|error| format!("unable to write progress: {error}"))
}

/// Reads a restart point written by [`write_progress`].
pub fn read_progress(text: &str) -> Option<(u64, u64)> {
    let mut lines_consumed = None;
    let mut records = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("lines_consumed=") {
            lines_consumed = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("records=") {
            records = value.trim().parse().ok();
        }
    }
    Some((lines_consumed?, records?))
}

#[cfg(test)]
mod tests {
    use super::{
        ConvertConfig, ConvertStats, MATE_SATURATION_CP, ParsedLine, RawScore, SkipReason, convert,
        parse_line, parse_source_fen, pick_eval, read_progress, to_side_to_move_centipawns,
        write_progress,
    };
    use crate::filter::{Filter, Rejection};
    use crate::record::{Outcome, RECORD_BYTES, Record};
    use mf_core::Color;

    /// A real line from `lichess_db_eval.jsonl`, trimmed to two principal variations.
    const REAL_LINE: &str = r#"{"fen":"7r/1p3k2/p1bPR3/5p2/2B2P1p/8/PP4P1/3K4 b - -","evals":[{"pvs":[{"cp":69,"line":"f7g7 e6e2 h8d8 e2d2"},{"cp":163,"line":"h8d8 d1e1 a6a5 a2a3"}],"knodes":4189972,"depth":46}]}"#;

    /// A real mate line: white to move, `mate` positive, several evals at differing depths.
    const MATE_LINE: &str = r#"{"fen":"6k1/6p1/8/4K3/4NN2/8/8/8 w - -","evals":[{"pvs":[{"mate":15,"line":"e5e6 g8f8 e4d6"}],"knodes":589893,"depth":95},{"pvs":[{"mate":20,"line":"e4g5 g8f8 f4g6"}],"knodes":74318,"depth":34}]}"#;

    fn convert_all(source: &str, config: ConvertConfig) -> (Vec<u8>, ConvertStats) {
        let mut output = Vec::new();
        let stats = convert(
            source.as_bytes(),
            config,
            |bytes| {
                output.extend_from_slice(bytes);
                Ok(())
            },
            |_| Ok(()),
        )
        .expect("conversion succeeds");
        (output, stats)
    }

    #[test]
    fn a_real_source_line_yields_its_fen_and_deepest_eval() {
        let ParsedLine { fen, eval } = parse_line(REAL_LINE).expect("line parses");
        assert_eq!(fen, "7r/1p3k2/p1bPR3/5p2/2B2P1p/8/PP4P1/3K4 b - -");
        assert_eq!(eval.depth, 46);
        assert_eq!(eval.knodes, 4_189_972);
        assert_eq!(eval.score, RawScore::Cp(69));
        assert_eq!(
            eval.first_move, "f7g7",
            "the first move of the FIRST pv is the position's best move"
        );
    }

    #[test]
    fn the_deepest_eval_wins_regardless_of_its_position_in_the_array() {
        let eval = parse_line(MATE_LINE).expect("line parses").eval;
        assert_eq!(eval.depth, 95, "depth 95 beats depth 34");
        assert_eq!(eval.score, RawScore::Mate(15));
    }

    #[test]
    fn equal_depths_are_broken_by_knodes_and_then_by_file_order() {
        let body = r#"{"pvs":[{"cp":10,"line":"e2e4"}],"knodes":100,"depth":20},
                      {"pvs":[{"cp":20,"line":"d2d4"}],"knodes":900,"depth":20},
                      {"pvs":[{"cp":30,"line":"c2c4"}],"knodes":900,"depth":20}]"#;
        let eval = pick_eval(body).expect("an eval is picked");
        assert_eq!(
            eval.score,
            RawScore::Cp(20),
            "greater knodes wins at equal depth, and a full tie keeps the earlier entry"
        );
    }

    #[test]
    fn a_brace_inside_a_quoted_value_does_not_open_an_object_level() {
        let body = r#"{"pvs":[{"cp":5,"line":"e2e4 {not-a-brace"}],"knodes":1,"depth":9}]"#;
        let eval = pick_eval(body).expect("an eval is picked");
        assert_eq!(eval.depth, 9);
        assert_eq!(eval.first_move, "e2e4");
    }

    #[test]
    fn a_line_with_no_evals_or_no_fen_parses_to_nothing_rather_than_panicking() {
        assert!(parse_line(r#"{"fen":"8/8/8/8/8/8/8/8 w - -","evals":[]}"#).is_none());
        assert!(parse_line(r#"{"evals":[{"pvs":[{"cp":1,"line":"e2e4"}],"depth":2}]}"#).is_none());
        assert!(parse_line("not json at all").is_none());
        assert!(parse_line("").is_none());
    }

    #[test]
    fn the_four_field_source_fen_is_completed_with_the_canonical_clocks() {
        let position =
            parse_source_fen("7r/1p3k2/p1bPR3/5p2/2B2P1p/8/PP4P1/3K4 b - -").expect("FEN parses");
        assert_eq!(position.side_to_move(), Color::Black);
        assert!(parse_source_fen("8/8/8/8 w").is_none(), "too few fields");
        assert!(
            parse_source_fen("not/a/board w - - 0 1").is_none(),
            "a six-field FEN is accepted but must still be a legal board"
        );
    }

    #[test]
    fn white_relative_scores_are_negated_for_black_to_move() {
        // Determined empirically against Stockfish: 29/30 sign agreement under the
        // white-relative hypothesis, 0/30 under side-to-move relative.
        assert_eq!(
            to_side_to_move_centipawns(RawScore::Cp(200), Color::White, 10_000),
            (200, false)
        );
        assert_eq!(
            to_side_to_move_centipawns(RawScore::Cp(200), Color::Black, 10_000),
            (-200, false)
        );
    }

    #[test]
    fn mate_announcements_saturate_to_a_signed_centipawn_value_and_are_flagged() {
        assert_eq!(
            to_side_to_move_centipawns(RawScore::Mate(3), Color::White, 10_000),
            (10_000, true)
        );
        assert_eq!(
            to_side_to_move_centipawns(RawScore::Mate(-3), Color::White, 10_000),
            (-10_000, true)
        );
        assert_eq!(
            to_side_to_move_centipawns(RawScore::Mate(3), Color::Black, 10_000),
            (-10_000, true),
            "a mate for white is a loss for black to move"
        );
    }

    #[test]
    fn the_mate_saturation_is_clamped_to_the_score_bound_so_mates_are_not_all_rejected() {
        let config = ConvertConfig {
            filter: Filter { score_bound: 600 },
            ..ConvertConfig::default()
        };
        assert_eq!(config.effective_mate_cp(), 600);
        let (_, stats) = convert_all(MATE_LINE, config);
        assert_eq!(stats.positions, 1, "the mate must survive a low bound");
        assert_eq!(stats.rejected[Rejection::ScoreOutOfBounds as usize], 0);
    }

    #[test]
    fn a_converted_record_is_byte_identical_to_encoding_the_position_directly() {
        let (bytes, stats) = convert_all(REAL_LINE, ConvertConfig::default());
        assert_eq!(stats.positions, 1);
        assert_eq!(bytes.len(), RECORD_BYTES);

        let position =
            parse_source_fen("7r/1p3k2/p1bPR3/5p2/2B2P1p/8/PP4P1/3K4 b - -").expect("FEN parses");
        // Black to move and the source says +69 for white, so the record must hold -69.
        let expected = Record::encode(&position, -69, Outcome::Draw).expect("encodes");
        assert_eq!(bytes, expected.to_bytes());
    }

    #[test]
    fn every_record_carries_the_neutral_draw_placeholder() {
        let source = format!("{REAL_LINE}\n{MATE_LINE}\n");
        let (bytes, stats) = convert_all(&source, ConvertConfig::default());
        assert_eq!(stats.positions, 2);
        assert_eq!(stats.mate_converted, 1, "exactly one came from a mate");
        for chunk in bytes.chunks_exact(RECORD_BYTES) {
            let record = Record::from_bytes(chunk.try_into().expect("chunk is exactly one record"));
            assert_eq!(
                record.outcome(),
                Some(Outcome::Draw),
                "the source has no game result; the placeholder must be neutral"
            );
            assert_eq!(record.structural_errors(), Vec::new());
        }
    }

    #[test]
    fn the_position_filters_still_apply_to_downloaded_data() {
        // Black to move and in check along h5-g6-f7-e8; Rg6 is a legal quiet block, so
        // the position reaches the filter rather than being skipped as a bad PV move.
        let in_check = r#"{"fen":"4k3/8/8/7Q/8/6r1/8/4K3 b - -","evals":[{"pvs":[{"cp":-50,"line":"g3g6"}],"knodes":10,"depth":30}]}"#;
        let (bytes, stats) = convert_all(in_check, ConvertConfig::default());
        assert!(bytes.is_empty());
        assert_eq!(stats.rejected[Rejection::InCheck as usize], 1);
        assert_eq!(stats.considered, 1);
    }

    #[test]
    fn a_position_whose_best_move_is_tactical_is_dropped_using_the_pv_first_move() {
        let capture = r#"{"fen":"rnbqkbnr/ppp1pppp/8/3p4/4P3/8/PPPP1PPP/RNBQKBNR w KQkq -","evals":[{"pvs":[{"cp":15,"line":"e4d5 d8d5"}],"knodes":10,"depth":30}]}"#;
        let (bytes, stats) = convert_all(capture, ConvertConfig::default());
        assert!(bytes.is_empty());
        assert_eq!(stats.rejected[Rejection::TacticalMove as usize], 1);
    }

    #[test]
    fn castling_written_as_king_takes_rook_is_accepted_and_kept() {
        // The source writes castling in king-takes-rook form even for standard chess.
        // Parsing standard-only drops the move as illegal, which would silently strip
        // the corpus of its king-safety positions: 638 of 28,633 sample lines, every
        // one of them a castle.
        let source = r#"{"fen":"r1bq1rk1/pp2bppp/2n1pn2/6B1/2BP4/2N2N2/PP3PPP/R2QK2R w KQ -","evals":[{"pvs":[{"cp":24,"line":"e1h1 h7h6"}],"knodes":5000,"depth":36}]}"#;
        let (bytes, stats) = convert_all(source, ConvertConfig::default());
        assert_eq!(
            stats.skipped[SkipReason::BadPvMove as usize],
            0,
            "king-takes-rook castling must not read as an illegal move"
        );
        assert_eq!(stats.positions, 1, "castling is KEPT, not filtered");
        assert_eq!(bytes.len(), RECORD_BYTES);
        assert_eq!(stats.rejected[Rejection::TacticalMove as usize], 0);
    }

    #[test]
    fn malformed_lines_are_counted_by_reason_rather_than_aborting_the_run() {
        let source = format!(
            "not json\n{}\n{}\n{}\n{REAL_LINE}\n",
            r#"{"fen":"8/8/8/8/8/8/8/8 w - -","evals":[]}"#,
            r#"{"fen":"nonsense w - -","evals":[{"pvs":[{"cp":1,"line":"e2e4"}],"knodes":1,"depth":9}]}"#,
            r#"{"fen":"7r/1p3k2/p1bPR3/5p2/2B2P1p/8/PP4P1/3K4 b - -","evals":[{"pvs":[{"cp":1,"line":"a1a8"}],"knodes":1,"depth":9}]}"#,
        );
        let (bytes, stats) = convert_all(&source, ConvertConfig::default());
        assert_eq!(stats.lines, 5);
        assert_eq!(stats.positions, 1);
        assert_eq!(bytes.len(), RECORD_BYTES);
        assert_eq!(stats.skipped[SkipReason::Unparseable as usize], 1);
        assert_eq!(stats.skipped[SkipReason::NoEval as usize], 1);
        assert_eq!(stats.skipped[SkipReason::BadFen as usize], 1);
        assert_eq!(
            stats.skipped[SkipReason::BadPvMove as usize],
            1,
            "a1a8 is not legal in that position"
        );
    }

    #[test]
    fn skipping_lines_resumes_exactly_where_a_previous_run_stopped() {
        let source = format!("{REAL_LINE}\n{MATE_LINE}\n");
        let (whole, whole_stats) = convert_all(&source, ConvertConfig::default());
        assert_eq!(whole_stats.positions, 2);

        let (first, first_stats) = convert_all(
            &source,
            ConvertConfig {
                max_positions: Some(1),
                ..ConvertConfig::default()
            },
        );
        assert_eq!(first_stats.positions, 1);

        let (rest, rest_stats) = convert_all(
            &source,
            ConvertConfig {
                skip_lines: first_stats.lines_consumed,
                ..ConvertConfig::default()
            },
        );
        assert_eq!(rest_stats.positions, 1);

        let mut rejoined = first;
        rejoined.extend_from_slice(&rest);
        assert_eq!(
            rejoined, whole,
            "a restart must reproduce the uninterrupted corpus byte for byte"
        );
    }

    #[test]
    fn a_progress_marker_round_trips() {
        let stats = ConvertStats {
            lines_consumed: 1234,
            positions: 987,
            ..ConvertStats::default()
        };
        let mut text = Vec::new();
        write_progress(&mut text, &stats).expect("writes");
        let text = String::from_utf8(text).expect("UTF-8");
        assert_eq!(read_progress(&text), Some((1234, 987)));
        assert_eq!(read_progress("garbage"), None);
    }

    #[test]
    fn the_default_mate_saturation_matches_the_default_score_bound() {
        assert_eq!(MATE_SATURATION_CP, crate::DEFAULT_SCORE_BOUND);
        assert_eq!(ConvertConfig::default().effective_mate_cp(), 10_000);
    }
}
