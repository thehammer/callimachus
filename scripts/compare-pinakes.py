#!/usr/bin/env python3
"""compare-pinakes.py — diff N pinakes produced by different generation methods.

Usage:
    scripts/compare-pinakes.py --corpus callimachus \\
        --pinakes A=data/callimachus-A.pinakes \\
        --pinakes B=data/callimachus-B.pinakes \\
        [--pinakes M=data/callimachus.pinakes]

Emits a Markdown report to stdout. Three sections so far:

  1. HEAD deterministic diff  — chunks/entities/edges set membership
  2. Per-SHA history coverage — how many entities does VirtualHead return at
                                each first-parent ancestor for each pinakes
  3. Entity-kind breakdown    — what kinds of entities differ at HEAD

Designed to take N pinakes (≥2). Same code path for A vs B today and
A vs B vs M tomorrow.
"""

from __future__ import annotations

import argparse
import sqlite3
import subprocess
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


# ─── pinakes loading ─────────────────────────────────────────────────────────


@dataclass
class Pinakes:
    label: str
    path: Path
    conn: sqlite3.Connection

    @classmethod
    def open(cls, label: str, path: Path) -> "Pinakes":
        if not path.exists():
            sys.exit(f"error: pinakes not found at {path}")
        conn = sqlite3.connect(str(path))
        return cls(label=label, path=path, conn=conn)

    def all(self, sql: str, params: tuple = ()) -> list[tuple]:
        return self.conn.execute(sql, params).fetchall()

    def one(self, sql: str, params: tuple = ()):
        row = self.conn.execute(sql, params).fetchone()
        return row[0] if row else None


def parse_pinakes_arg(spec: str) -> tuple[str, Path]:
    if "=" not in spec:
        sys.exit(f"error: --pinakes expects LABEL=PATH (got {spec!r})")
    label, _, path = spec.partition("=")
    return label, Path(path).resolve()


# ─── git first-parent ancestry ───────────────────────────────────────────────


def first_parent_ancestry(repo: Path) -> list[str]:
    """Return first-parent SHAs from root → HEAD (oldest first), prefixed
    `git:` to match the pinakes' `derived_at_version` format."""
    out = subprocess.check_output(
        ["git", "-C", str(repo), "rev-list", "--reverse", "--first-parent", "HEAD"],
        text=True,
    )
    return [f"git:{sha.strip()}" for sha in out.splitlines() if sha.strip()]


# ─── Phase 1: HEAD deterministic diff ────────────────────────────────────────


def fetch_head_ids(p: Pinakes, table: str, corpus: str) -> set[str]:
    """Pull the natural-key set for a table at HEAD."""
    if table == "chunks":
        rows = p.all(
            "SELECT id FROM chunks WHERE corpus_id = ?", (corpus,)
        )
    elif table == "entities":
        rows = p.all(
            "SELECT id FROM entities WHERE corpus_id = ?", (corpus,)
        )
    elif table == "edges":
        # Edge identity for comparison purposes = (from, to, kind).
        # The `id` column is a UUID generated at insert and won't match
        # across pinakes even when the logical edge is identical.
        rows = p.all(
            "SELECT from_entity_id || '→' || to_entity_id || '/' || kind "
            "FROM edges WHERE corpus_id = ?",
            (corpus,),
        )
    elif table == "themes":
        rows = p.all(
            "SELECT id FROM themes WHERE corpus_id = ?", (corpus,)
        )
    else:
        rows = []
    return {r[0] for r in rows}


def head_diff_section(corpus: str, pinakes: list[Pinakes]) -> str:
    lines = ["## 1. HEAD deterministic diff", ""]
    lines.append(
        "Set membership of natural keys at HEAD across pinakes. For deterministic"
    )
    lines.append(
        "tables (chunks/entities/edges), identical generation should yield"
    )
    lines.append("identical sets. Differences are findings, not noise.\n")

    for table in ("chunks", "entities", "edges", "themes"):
        lines.append(f"### {table}\n")
        sets = {p.label: fetch_head_ids(p, table, corpus) for p in pinakes}
        common = set.intersection(*sets.values()) if sets else set()

        rows = ["| pinakes | total | unique to this | shared with all |"]
        rows.append("|---|---:|---:|---:|")
        for label, s in sets.items():
            others = set.union(*(v for k, v in sets.items() if k != label)) if len(sets) > 1 else set()
            unique = s - others
            rows.append(
                f"| **{label}** | {len(s):,} | {len(unique):,} | {len(common):,} |"
            )
        lines.extend(rows)
        lines.append("")

        # Show a sample of "unique to one pinakes" entries if any
        for label, s in sets.items():
            others = set.union(*(v for k, v in sets.items() if k != label)) if len(sets) > 1 else set()
            unique = sorted(s - others)
            if unique:
                sample = unique[:5]
                lines.append(f"_{label}-only sample ({len(unique):,} total):_ "
                             + ", ".join(f"`{u[:60]}`" for u in sample))
                lines.append("")

    return "\n".join(lines)


# ─── Phase 2: per-SHA history coverage ──────────────────────────────────────


def virtualhead_entity_count(p: Pinakes, corpus: str, sha: str) -> int:
    """Mirror of VirtualHead.entity_count: head ∪ history WHERE derived_at_version = sha."""
    n = p.one(
        """
        SELECT
            (SELECT COUNT(*) FROM entities
              WHERE corpus_id = ? AND derived_at_version = ?)
          + (SELECT COUNT(*) FROM entities_history
              WHERE corpus_id = ? AND derived_at_version = ?)
        """,
        (corpus, sha, corpus, sha),
    )
    return int(n or 0)


def coverage_section(
    corpus: str, pinakes: list[Pinakes], ancestry: list[str]
) -> str:
    lines = ["## 2. Per-SHA history coverage", ""]
    lines.append(
        "For each first-parent commit, count entities returned by the exact-"
    )
    lines.append(
        "SHA-match query (`WHERE derived_at_version = SHA` across head ∪ history)."
    )
    lines.append(
        "This is what `VirtualHead.entity_list` returns. Differences across"
    )
    lines.append(
        "pinakes show how each generation method represents historical state.\n"
    )
    lines.append(
        "**Interpretation:** if a pinakes returns ~the full corpus entity count"
    )
    lines.append(
        "at most SHAs, it satisfies the exact-SHA-match invariant (every"
    )
    lines.append(
        "commit has a complete stamped artifact set). If it returns small"
    )
    lines.append(
        "supersession-only counts at most SHAs, it relies on cascade-archival"
    )
    lines.append(
        "and only stamps entities at commits where their source file changed.\n"
    )

    header = ["sha"] + [p.label for p in pinakes]
    aligns = [":---"] + ["---:"] * len(pinakes)
    lines.append("| " + " | ".join(header) + " |")
    lines.append("|" + "|".join(aligns) + "|")

    # Aggregate stats
    stats = {p.label: [] for p in pinakes}

    # Sample of first 5, last 5, and middle commits
    indices_to_show = set()
    n = len(ancestry)
    if n <= 12:
        indices_to_show = set(range(n))
    else:
        indices_to_show |= set(range(5))            # first 5
        indices_to_show |= set(range(n - 5, n))     # last 5
        indices_to_show |= {n // 2}                 # middle

    for i, sha in enumerate(ancestry):
        counts = {p.label: virtualhead_entity_count(p, corpus, sha) for p in pinakes}
        for label, c in counts.items():
            stats[label].append(c)
        if i in indices_to_show:
            display_sha = sha.removeprefix("git:")[:8]
            row = [f"`{display_sha}`"] + [f"{counts[p.label]:,}" for p in pinakes]
            lines.append("| " + " | ".join(row) + " |")

    lines.append("")
    lines.append(
        f"_Showing {len(indices_to_show)} of {n} first-parent commits "
        "(first 5, middle, last 5)._\n"
    )

    # Aggregate stats
    lines.append("### Coverage summary across all SHAs\n")
    lines.append("| pinakes | min | median | max | mean | SHAs with ≥100 entities |")
    lines.append("|---|---:|---:|---:|---:|---:|")
    for label, counts in stats.items():
        if not counts:
            continue
        sorted_c = sorted(counts)
        median = sorted_c[len(sorted_c) // 2]
        mean = sum(counts) / len(counts)
        with_100 = sum(1 for c in counts if c >= 100)
        lines.append(
            f"| **{label}** | {min(counts):,} | {median:,} | {max(counts):,} "
            f"| {mean:,.1f} | {with_100}/{len(counts)} |"
        )

    return "\n".join(lines)


# ─── Phase 3: entity-kind breakdown at HEAD ─────────────────────────────────


def kind_breakdown_section(corpus: str, pinakes: list[Pinakes]) -> str:
    lines = ["## 3. HEAD entity-kind breakdown", ""]
    lines.append(
        "Counts of entities by kind. Useful for understanding the shape of"
    )
    lines.append(
        "any HEAD discrepancy — e.g. orphans from rename cascades typically"
    )
    lines.append("show up as inflated `function` or `class` counts.\n")

    kinds = set()
    by_pinakes = {}
    for p in pinakes:
        rows = p.all(
            "SELECT kind, COUNT(*) FROM entities WHERE corpus_id = ? GROUP BY kind",
            (corpus,),
        )
        by_pinakes[p.label] = dict(rows)
        kinds |= set(by_pinakes[p.label].keys())

    header = ["kind"] + [p.label for p in pinakes] + ["max − min"]
    aligns = [":---"] + ["---:"] * (len(pinakes) + 1)
    lines.append("| " + " | ".join(header) + " |")
    lines.append("|" + "|".join(aligns) + "|")

    for kind in sorted(kinds):
        vals = [by_pinakes[p.label].get(kind, 0) for p in pinakes]
        delta = max(vals) - min(vals)
        row = [f"`{kind}`"] + [f"{v:,}" for v in vals] + [f"{delta:,}"]
        lines.append("| " + " | ".join(row) + " |")

    return "\n".join(lines)


# ─── main ──────────────────────────────────────────────────────────────────


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--corpus",
        required=True,
        help="Corpus id (e.g. callimachus)",
    )
    parser.add_argument(
        "--pinakes",
        action="append",
        required=True,
        help="LABEL=PATH (repeat for N pinakes)",
    )
    parser.add_argument(
        "--repo",
        type=Path,
        default=Path.cwd(),
        help="Git repo to read first-parent ancestry from (default: cwd)",
    )
    args = parser.parse_args()

    pinakes: list[Pinakes] = []
    for spec in args.pinakes:
        label, path = parse_pinakes_arg(spec)
        pinakes.append(Pinakes.open(label, path))

    if len(pinakes) < 2:
        sys.exit("error: need at least 2 --pinakes")

    ancestry = first_parent_ancestry(args.repo)

    print(f"# Pinakes comparison: {' vs '.join(p.label for p in pinakes)}")
    print()
    print(f"- **Corpus:** `{args.corpus}`")
    print(f"- **Git repo:** `{args.repo}`")
    print(f"- **First-parent commits:** {len(ancestry)} ({ancestry[0][:12]}… → {ancestry[-1][:12]}…)")
    for p in pinakes:
        size_mb = p.path.stat().st_size / 1024 / 1024
        print(f"- **{p.label}:** `{p.path.name}` ({size_mb:.1f} MB)")
    print()

    print(head_diff_section(args.corpus, pinakes))
    print()
    print(kind_breakdown_section(args.corpus, pinakes))
    print()
    print(coverage_section(args.corpus, pinakes, ancestry))


if __name__ == "__main__":
    main()
