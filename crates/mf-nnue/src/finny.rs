//! Finny tables: a per-thread cache of HalfKAv2_hm accumulators keyed by king square.
//!
//! A king move invalidates every HalfKA feature index for that perspective, because the index
//! carries the king's bucket and mirror orientation. The engine's answer was to rebuild the
//! whole perspective from scratch, which the M2-F1 profile measured at 91.4 rebuilds per 1000
//! searched nodes and ~1471 ns each.
//!
//! This cache holds, for each `(perspective, king square)`, the HalfKA-only accumulator of the
//! last position seen with that key, together with the piece bitboards that produced it. A
//! refresh diffs the requested position against those bitboards and applies only the differing
//! rows, so a king move costs a handful of row updates instead of one per piece on the board.
//!
//! FullThreats rows are deliberately **not** cached here; see `AccumulatorState::update_from`
//! for why the threat contribution needs no cache at all.

use mf_core::{Bitboard, Color, Piece, PieceKind, Position, Square};

use crate::halfka;
#[cfg(feature = "instrumentation")]
use crate::instrumentation;
use crate::network::{L1, Network, PSQT_BUCKETS};
use crate::simd::{SimdBackend, add_i16_row, add_psqt_row, subtract_i16_row, subtract_psqt_row};

use crate::accumulator::prefetch_half_ka_rows;

/// Distinct `(color, kind)` piece slots tracked per cache entry.
const PIECE_SLOTS: usize = 12;

/// One entry per `(perspective, king square)`.
///
/// Keying by king square rather than by HalfKA bucket is required, not conservative: two king
/// squares yield identical HalfKA indices only when they share both the bucket offset and the
/// mirror orientation, and `WHITE_KING_BUCKETS` is mirror-symmetric with 32 distinct values, so
/// that pair is unique per square.
const ENTRIES: usize = 2 * 64;

/// A position has at most 32 pieces, so a diff against any other position moves at most 32 rows
/// in each direction.
const MAX_DELTA: usize = 32;

/// A cached HalfKA-only accumulator and the position that produced it.
#[repr(C, align(64))]
#[derive(Clone)]
pub(crate) struct HalfKaEntry {
    pub(crate) values: [i16; L1],
    pub(crate) psqt: [i32; PSQT_BUCKETS],
    pieces: [u64; PIECE_SLOTS],
}

/// Per-search-thread Finny table.
pub(crate) struct FinnyTable {
    entries: Box<[HalfKaEntry]>,
}

impl FinnyTable {
    /// Builds a table whose entries all describe an empty board.
    ///
    /// An empty board is a genuine HalfKA accumulator — the feature-transformer biases with no
    /// piece rows added — so the first refresh of a cold entry adds every piece and costs exactly
    /// what the rebuild it replaces cost. No entry is ever invalid, only stale.
    pub(crate) fn new(network: &Network) -> Self {
        let empty = HalfKaEntry {
            values: *network.feature_transformer_biases(),
            psqt: [0; PSQT_BUCKETS],
            pieces: [0; PIECE_SLOTS],
        };
        Self {
            entries: vec![empty; ENTRIES].into_boxed_slice(),
        }
    }

    /// Brings the entry for `position`'s king into sync with `position` and returns its index.
    ///
    /// Returns an index rather than a reference so callers can refresh two entries and then read
    /// both, which the two-perspective swap in `update_from` needs.
    pub(crate) fn refresh(
        &mut self,
        network: &Network,
        backend: SimdBackend,
        perspective: Color,
        position: &Position,
    ) -> usize {
        let king_square = position.king_square(perspective);
        let index = entry_index(perspective, king_square);
        let entry = &mut self.entries[index];

        let mut removals = [0_usize; MAX_DELTA];
        let mut additions = [0_usize; MAX_DELTA];
        let mut removed = 0;
        let mut added = 0;

        for color in Color::ALL {
            for kind in PieceKind::ALL {
                let slot = color.index() * PieceKind::ALL.len() + kind.index();
                let current = position.pieces(color, kind).bits();
                let cached = entry.pieces[slot];
                if current == cached {
                    continue;
                }
                let piece = Piece::new(color, kind);
                let mut gone = Bitboard::new(cached & !current);
                while let Some(square) = gone.pop_first() {
                    removals[removed] = halfka::make_index(perspective, piece, square, king_square);
                    removed += 1;
                }
                let mut arrived = Bitboard::new(current & !cached);
                while let Some(square) = arrived.pop_first() {
                    additions[added] = halfka::make_index(perspective, piece, square, king_square);
                    added += 1;
                }
                entry.pieces[slot] = current;
            }
        }

        prefetch_half_ka_rows(network, &removals[..removed]);
        prefetch_half_ka_rows(network, &additions[..added]);

        for &feature in &removals[..removed] {
            subtract_i16_row(
                backend,
                &mut entry.values,
                network
                    .half_ka_weights()
                    .row(feature)
                    .expect("cached HalfKA removal must be in range"),
            );
            subtract_psqt_row(
                backend,
                &mut entry.psqt,
                network
                    .psqt_row(feature)
                    .expect("cached HalfKA PSQT removal must be in range"),
            );
        }
        for &feature in &additions[..added] {
            add_i16_row(
                backend,
                &mut entry.values,
                network
                    .half_ka_weights()
                    .row(feature)
                    .expect("cached HalfKA addition must be in range"),
            );
            add_psqt_row(
                backend,
                &mut entry.psqt,
                network
                    .psqt_row(feature)
                    .expect("cached HalfKA PSQT addition must be in range"),
            );
        }

        #[cfg(feature = "instrumentation")]
        {
            let rows = (removed + added) as u64;
            instrumentation::record(|counters| {
                counters.finny_refreshes += 1;
                counters.finny_delta_rows += rows;
            });
        }

        index
    }

    /// Reads a refreshed entry.
    pub(crate) fn entry(&self, index: usize) -> &HalfKaEntry {
        &self.entries[index]
    }
}

#[inline]
fn entry_index(perspective: Color, king_square: Square) -> usize {
    perspective.index() * 64 + usize::from(king_square.index())
}

#[cfg(test)]
mod tests {
    use mf_core::{Color, Position};

    use super::{ENTRIES, FinnyTable, HalfKaEntry, MAX_DELTA};
    use crate::halfka;
    use crate::network::{L1, Network, PSQT_BUCKETS};
    use crate::simd::SimdBackend;
    use crate::test_support::local_network;

    /// Independent HalfKA-only oracle: biases plus one row per piece, no threat rows.
    fn halfka_only_oracle(
        network: &Network,
        position: &Position,
        perspective: Color,
    ) -> ([i16; L1], [i32; PSQT_BUCKETS]) {
        let mut values = *network.feature_transformer_biases();
        let mut psqt = [0; PSQT_BUCKETS];
        let king_square = position.king_square(perspective);
        for square in position.occupancy() {
            let piece = position
                .piece_at(square)
                .expect("occupied square has a piece");
            let feature = halfka::make_index(perspective, piece, square, king_square);
            for (value, &weight) in values
                .iter_mut()
                .zip(network.half_ka_weights().row(feature).expect("in range"))
            {
                *value = value.wrapping_add(weight);
            }
            for (value, &weight) in psqt
                .iter_mut()
                .zip(network.psqt_row(feature).expect("in range"))
            {
                *value = i32::wrapping_add(*value, weight);
            }
        }
        (values, psqt)
    }

    #[test]
    fn entries_cover_every_king_square_for_both_perspectives() {
        assert_eq!(ENTRIES, 128);
        assert_eq!(MAX_DELTA, 32);
        assert_eq!(core::mem::align_of::<HalfKaEntry>(), 64);
    }

    /// The table is allocated per search thread, so its size is multiplied by `Threads`.
    #[test]
    fn the_table_stays_small_enough_to_hold_one_per_search_thread() {
        assert_eq!(core::mem::size_of::<HalfKaEntry>(), 2_176);
        let bytes = core::mem::size_of::<HalfKaEntry>() * ENTRIES;
        assert_eq!(bytes, 278_528);
        // Comfortably under a mebibyte per thread, so even 64 threads cost only ~17 MiB.
        assert!(bytes < 1 << 20);
    }

    #[test]
    fn a_cold_refresh_reproduces_the_halfka_only_accumulator() {
        let Some(network) = local_network("cold Finny refresh test") else {
            return;
        };
        let position = Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            false,
        )
        .expect("cold-refresh FEN should parse");
        let mut table = FinnyTable::new(&network);

        for perspective in Color::ALL {
            let index = table.refresh(&network, SimdBackend::Scalar, perspective, &position);
            let entry = table.entry(index);
            let (values, psqt) = halfka_only_oracle(&network, &position, perspective);
            assert_eq!(entry.values, values, "{perspective:?}");
            assert_eq!(entry.psqt, psqt, "{perspective:?}");
        }
    }

    #[test]
    fn a_warm_refresh_applies_only_the_difference_and_still_matches_the_oracle() {
        let Some(network) = local_network("warm Finny refresh test") else {
            return;
        };
        let mut table = FinnyTable::new(&network);
        let mut position = Position::startpos();

        // Walk a real game so successive refreshes of the same key see a drifting position.
        for notation in [
            "e2e4", "e7e5", "g1f3", "b8c6", "f1b5", "a7a6", "b5a4", "g8f6", "e1g1", "f8e7",
        ] {
            let mv = mf_core::parse_uci_move(&position, notation, false)
                .expect("warm-refresh move should be legal");
            position.make_move(mv);
            for perspective in Color::ALL {
                let index = table.refresh(&network, SimdBackend::Scalar, perspective, &position);
                let entry = table.entry(index);
                let (values, psqt) = halfka_only_oracle(&network, &position, perspective);
                assert_eq!(entry.values, values, "{notation} {perspective:?}");
                assert_eq!(entry.psqt, psqt, "{notation} {perspective:?}");
            }
        }
    }

    #[test]
    fn refreshing_the_same_key_from_an_unrelated_position_stays_exact() {
        let Some(network) = local_network("stale Finny refresh test") else {
            return;
        };
        let mut table = FinnyTable::new(&network);
        // Same white king square (e1), wildly different material and black king square.
        let positions = [
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            "4k3/8/8/8/8/8/8/4K3 w - - 0 1",
            // The FEN this replaced had the white king in check with black to move
            // (the b6 bishop bears on g1), an unreachable position. Kiwipete keeps the
            // "wildly different material" property the refresh must survive.
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/8/8/2k5/8/8/PPP5/4K3 w - - 0 1",
        ];

        for fen in positions {
            let position = Position::from_fen(fen, false).expect("stale-refresh FEN should parse");
            let index = table.refresh(&network, SimdBackend::Scalar, Color::White, &position);
            let entry = table.entry(index);
            let (values, psqt) = halfka_only_oracle(&network, &position, Color::White);
            assert_eq!(entry.values, values, "{fen}");
            assert_eq!(entry.psqt, psqt, "{fen}");
        }
    }
}
