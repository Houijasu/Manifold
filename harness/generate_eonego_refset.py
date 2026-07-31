#!/usr/bin/env python3
"""Generate the deterministic 10,000-position Eonego NNUE parity corpus."""

from __future__ import annotations

import argparse
import random
import sys
from collections import Counter
from pathlib import Path

import chess


LINE_COUNT = 10_000
SEED = 0xE0E60
EXPECTED_CHESS_VERSION = "1.11.2"
EXPECTED_PYTHON = (3, 12)
DEFAULT_OUTPUT = (
    Path(__file__).resolve().parents[1]
    / "crates"
    / "mf-nnue"
    / "tests"
    / "data"
    / "eonego_refset_10k.fen"
)

# Keep these first and in this order so failures on named edge cases have stable
# record indices. X-FEN rights require the Manifold test to enable Chess960
# parsing; Eonego intentionally ignores unknown castling-right letters.
EDGE_FENS = [
    # Both sides to move and standard castling rights.
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR b KQkq - 0 1",
    # A legal en-passant target with a real white capturer.
    "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR w KQkq f6 0 3",
    # A promotion-ready pawn and material that necessarily came from promotion.
    "4k3/P7/8/8/8/8/7p/4K3 w - - 0 1",
    "QQ2k3/8/8/8/8/8/7p/4K3 b - - 0 1",
    # Chess960 start position 0 with Shredder/X-FEN rook-file rights.
    "bbqnnrkr/pppppppp/8/8/8/8/PPPPPPPP/BBQNNRKR w HFhf - 0 1",
    # Sparse endgames.
    "8/8/8/3k4/8/3K4/4P3/8 w - - 0 1",
    "8/2k5/8/8/8/8/5K2/6R1 b - - 71 93",
    # One position in each occupancy bucket 0 through 7.
    "4k3/8/8/8/8/8/8/4K3 w - - 0 1",
    "2b1k3/2p1p3/8/8/8/8/2PPP1P1/4K3 w - - 0 1",
    "1n2kb1r/2pp3p/8/8/8/8/1P3PP1/4KBN1 w - - 0 1",
    "r1bqk2r/pp2pppp/8/8/8/8/2PPPPP1/4K3 w - - 0 1",
    "rnbqk1nr/p2ppp2/8/8/8/8/P2PP2P/1NBQKB1R w - - 0 1",
    "rn2k2r/pppppppp/8/8/8/8/PPPPPPPP/1NB1KBN1 w - - 0 1",
    "rnbqkb1r/pp1ppppp/8/8/8/8/PPPPPPPP/RNBQKBN1 w - - 0 1",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_OUTPUT,
        help=f"output path (default: {DEFAULT_OUTPUT})",
    )
    return parser.parse_args()


def occupancy_bucket(board: chess.Board) -> int:
    return (len(board.piece_map()) - 1) // 4


def validate_edge_fens() -> None:
    for index, fen in enumerate(EDGE_FENS):
        chess960 = any(symbol in fen.split()[2] for symbol in "ABCDEFGHabcdefgh")
        board = chess.Board(fen, chess960=chess960)
        if not board.is_valid():
            raise RuntimeError(
                f"edge FEN {index} is invalid (status={board.status()}): {fen}"
            )


def add_position(
    board: chess.Board, positions: list[str], seen: set[str]
) -> None:
    fen = board.fen(en_passant="fen")
    if fen not in seen:
        seen.add(fen)
        positions.append(fen)


def generate_positions() -> list[str]:
    if sys.version_info[:2] != EXPECTED_PYTHON:
        raise RuntimeError(
            "reproducible generation requires Python "
            f"{EXPECTED_PYTHON[0]}.{EXPECTED_PYTHON[1]}, "
            f"found {sys.version_info.major}.{sys.version_info.minor}"
        )
    if chess.__version__ != EXPECTED_CHESS_VERSION:
        raise RuntimeError(
            "reproducible generation requires python-chess "
            f"{EXPECTED_CHESS_VERSION}, found {chess.__version__}"
        )

    validate_edge_fens()
    positions = list(EDGE_FENS)
    seen = set(positions)
    if len(seen) != len(positions):
        raise RuntimeError("explicit edge FENs must be unique")

    rng = random.Random(SEED)
    while len(positions) < LINE_COUNT:
        board = chess.Board()
        max_plies = rng.randint(40, 240)

        for ply in range(max_plies):
            if board.is_game_over(claim_draw=False):
                break

            legal_moves = list(board.legal_moves)
            promotions = [move for move in legal_moves if move.promotion is not None]
            if promotions and rng.randrange(4) != 0:
                move = promotions[rng.randrange(len(promotions))]
            else:
                move = legal_moves[rng.randrange(len(legal_moves))]
            board.push(move)

            # Sample across the entire game, with denser late-game sampling so
            # all low-occupancy NNUE layer-stack buckets remain represented.
            sample_rate = 3 if ply < 40 else 2 if ply < 100 else 1
            if ply % sample_rate == 0:
                add_position(board, positions, seen)
                if len(positions) == LINE_COUNT:
                    break

    return positions


def main() -> int:
    args = parse_args()
    positions = generate_positions()
    if len(positions) != LINE_COUNT:
        raise RuntimeError(f"generated {len(positions)} FENs, expected {LINE_COUNT}")

    buckets = Counter(
        occupancy_bucket(
            chess.Board(
                fen,
                chess960=any(
                    symbol in fen.split()[2] for symbol in "ABCDEFGHabcdefgh"
                ),
            )
        )
        for fen in positions
    )
    missing = sorted(set(range(8)) - set(buckets))
    if missing:
        raise RuntimeError(f"missing occupancy buckets: {missing}")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(positions) + "\n", encoding="utf-8", newline="\n")

    line_count = len(args.out.read_text(encoding="utf-8").splitlines())
    if line_count != LINE_COUNT:
        raise RuntimeError(f"wrote {line_count} lines, expected {LINE_COUNT}")

    coverage = ", ".join(f"{bucket}={buckets[bucket]}" for bucket in range(8))
    print(f"Python: {sys.version_info.major}.{sys.version_info.minor}")
    print(f"python-chess: {chess.__version__}")
    print(f"seed: 0x{SEED:X}")
    print(f"wrote: {args.out}")
    print(f"line count: {line_count}")
    print(f"occupancy buckets: {coverage}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
