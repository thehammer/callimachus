#!/usr/bin/env python3
"""fetch-apple-docs.py — Fetch Apple developer documentation as markdown.

Enumerates top-level types from a Swift symbol graph, fetches the
corresponding DocC JSON from developer.apple.com, and renders each to a
Markdown file that the callimachus wiki adapter can ingest.

Usage:
    fetch-apple-docs.py \\
      --framework AppKit \\
      --framework Combine \\
      --framework Foundation \\
      --sdk /Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.sdk \\
      --output-dir data/apple-docs-macos-26-src

No third-party dependencies — only urllib, json, subprocess, argparse, pathlib, time.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

# ── Constants ─────────────────────────────────────────────────────────────────

TOP_LEVEL_KINDS = {
    "swift.class",
    "swift.struct",
    "swift.enum",
    "swift.protocol",
}

USER_AGENT = "callimachus-apple-docs-fetcher/1.0"

BASE_URL = "https://developer.apple.com/tutorials/data/documentation"

# ── Symbol graph extraction ───────────────────────────────────────────────────


def extract_symbol_graph(framework: str, sdk: str, target: str, tmpdir: Path) -> dict:
    """Run swift-symbolgraph-extract and return the parsed JSON."""
    cmd = [
        "xcrun",
        "swift-symbolgraph-extract",
        "-module-name", framework,
        "-target", target,
        "-sdk", sdk,
        "-output-dir", str(tmpdir),
    ]
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"error: swift-symbolgraph-extract failed for {framework}", file=sys.stderr)
        print(result.stderr, file=sys.stderr)
        sys.exit(1)

    graph_path = tmpdir / f"{framework}.symbols.json"
    if not graph_path.exists():
        print(f"error: symbol graph not found at {graph_path}", file=sys.stderr)
        sys.exit(1)

    with open(graph_path, encoding="utf-8") as f:
        return json.load(f)


def enumerate_top_level_types(graph: dict) -> list[dict]:
    """Return symbols that are top-level types (pathComponents length == 1)."""
    results = []
    for sym in graph.get("symbols", []):
        kind_id = sym.get("kind", {}).get("identifier", "")
        path_components = sym.get("pathComponents", [])
        if kind_id in TOP_LEVEL_KINDS and len(path_components) == 1:
            results.append(sym)
    return results


# ── HTTP fetch ────────────────────────────────────────────────────────────────


def fetch_json(url: str) -> dict | None:
    """Fetch JSON from url. Returns None on 404, raises on other errors."""
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise


# ── Markdown rendering ────────────────────────────────────────────────────────


def render_inline(nodes: list[dict], references: dict) -> str:
    """Render inlineContent nodes to a markdown string."""
    parts = []
    for node in nodes:
        t = node.get("type", "")
        if t == "text":
            parts.append(node.get("text", ""))
        elif t == "codeVoice":
            parts.append(f"`{node.get('code', '')}`")
        elif t == "emphasis":
            inner = render_inline(node.get("inlineContent", []), references)
            parts.append(f"*{inner}*")
        elif t == "strong":
            inner = render_inline(node.get("inlineContent", []), references)
            parts.append(f"**{inner}**")
        elif t == "reference":
            identifier = node.get("identifier", "")
            ref = references.get(identifier, {})
            title = ref.get("title") or identifier.rsplit("/", 1)[-1]
            parts.append(title)
        elif t == "image":
            variants = node.get("variants", [])
            url = variants[0].get("url", "") if variants else ""
            alt = node.get("alt", "image")
            if url:
                parts.append(f"![{alt}]({url})")
            # else drop
        else:
            # best-effort: try inlineContent or text
            inner_nodes = node.get("inlineContent", [])
            if inner_nodes:
                parts.append(render_inline(inner_nodes, references))
            elif "text" in node:
                parts.append(node["text"])
    return "".join(parts)


def render_list_items(items: list[dict], references: dict, ordered: bool) -> list[str]:
    """Render list items to markdown lines."""
    lines = []
    for i, item in enumerate(items):
        prefix = f"{i + 1}." if ordered else "-"
        content_nodes = item.get("content", [])
        item_text = render_content_nodes(content_nodes, references).strip()
        # Indent continuation lines
        first_line = True
        for line in item_text.splitlines():
            if first_line:
                lines.append(f"{prefix} {line}")
                first_line = False
            else:
                lines.append(f"  {line}")
    return lines


def render_content_nodes(nodes: list[dict], references: dict) -> str:
    """Render a list of block-level content nodes to markdown."""
    parts = []
    for node in nodes:
        t = node.get("type", "")
        if t == "heading":
            level = max(2, node.get("level", 2))
            hashes = "#" * level
            text = render_inline(node.get("content", []), references)
            parts.append(f"\n{hashes} {text}\n")
        elif t == "paragraph":
            text = render_inline(node.get("inlineContent", []), references)
            parts.append(f"\n{text}\n")
        elif t == "aside":
            style = node.get("style", "note").capitalize()
            inner = render_content_nodes(node.get("content", []), references).strip()
            # Prefix each line with "> "
            quoted = "\n".join(f"> {line}" if line else ">" for line in inner.splitlines())
            parts.append(f"\n> **{style}:**\n{quoted}\n")
        elif t == "codeListing":
            syntax = node.get("syntax", "swift") or "swift"
            code_lines = node.get("code", [])
            code = "\n".join(code_lines)
            parts.append(f"\n```{syntax}\n{code}\n```\n")
        elif t == "unorderedList":
            items = node.get("items", [])
            lines = render_list_items(items, references, ordered=False)
            parts.append("\n" + "\n".join(lines) + "\n")
        elif t == "orderedList":
            items = node.get("items", [])
            lines = render_list_items(items, references, ordered=True)
            parts.append("\n" + "\n".join(lines) + "\n")
        elif t == "links":
            # Inline as plain text — no link resolution in v1
            items = node.get("items", [])
            for item in items:
                identifier = item if isinstance(item, str) else item.get("identifier", "")
                ref = references.get(identifier, {})
                title = ref.get("title") or identifier.rsplit("/", 1)[-1]
                parts.append(f"\n- {title}\n")
        else:
            # best-effort fallback
            inline = node.get("inlineContent", [])
            if inline:
                text = render_inline(inline, references)
                parts.append(f"\n{text}\n")
            content = node.get("content", [])
            if content:
                parts.append(render_content_nodes(content, references))
    return "".join(parts)


def render_declaration(data: dict) -> str:
    """Extract and render the Swift declaration."""
    for section in data.get("primaryContentSections", []):
        if section.get("kind") != "declarations":
            continue
        for decl in section.get("declarations", []):
            tokens = decl.get("tokens", [])
            text = "".join(t.get("text", "") for t in tokens)
            if text.strip():
                return f"\n## Declaration\n\n```swift\n{text.strip()}\n```\n"
    return ""


def render_discussion(data: dict, references: dict) -> str:
    """Render primaryContentSections of kind 'content'."""
    parts = []
    for section in data.get("primaryContentSections", []):
        if section.get("kind") != "content":
            continue
        content = section.get("content", [])
        rendered = render_content_nodes(content, references)
        if rendered.strip():
            parts.append(f"\n## Discussion\n{rendered}")
    return "".join(parts)


def render_topics(data: dict, references: dict) -> str:
    """Render topicSections as a Topics block."""
    topic_sections = data.get("topicSections", [])
    if not topic_sections:
        return ""

    parts = ["\n## Topics\n"]
    for section in topic_sections:
        title = section.get("title", "")
        if title:
            parts.append(f"\n### {title}\n")
        identifiers = section.get("identifiers", [])
        for identifier in identifiers:
            ref = references.get(identifier, {})
            title_ref = ref.get("title") or identifier.rsplit("/", 1)[-1]
            abstract_nodes = ref.get("abstract", [])
            if abstract_nodes:
                abstract_text = render_inline(abstract_nodes, references)
                parts.append(f"- **{title_ref}** — {abstract_text}\n")
            else:
                parts.append(f"- **{title_ref}**\n")
    return "".join(parts)


def render_page(data: dict, framework: str, url: str) -> str:
    """Render a DocC JSON page to markdown."""
    references = data.get("references", {})
    metadata = data.get("metadata", {})

    title = metadata.get("title", "")
    symbol_kind = metadata.get("symbolKind", "")

    # Abstract
    abstract_nodes = data.get("abstract", [])
    abstract_text = render_inline(abstract_nodes, references).strip()

    # Build the document
    lines = [f"# {title}\n"]
    if symbol_kind:
        lines.append(f"**Kind:** {symbol_kind.capitalize()}")
    lines.append(f"**Framework:** {framework}")
    lines.append(f"**Source URL:** {url}")
    lines.append("")

    if abstract_text:
        lines.append(abstract_text)
        lines.append("")

    doc = "\n".join(lines)
    doc += render_declaration(data)
    doc += render_discussion(data, references)
    doc += render_topics(data, references)

    return doc


# ── Main ──────────────────────────────────────────────────────────────────────


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Fetch Apple developer docs as markdown for callimachus ingestion.",
    )
    parser.add_argument(
        "--framework",
        action="append",
        dest="frameworks",
        metavar="NAME",
        required=True,
        help="Framework name (repeatable, e.g. --framework AppKit --framework Combine)",
    )
    parser.add_argument(
        "--sdk",
        required=True,
        metavar="PATH",
        help="Path to the .sdk directory (e.g. /Applications/Xcode.app/.../MacOSX26.sdk)",
    )
    parser.add_argument(
        "--target",
        default="arm64-apple-macos26",
        metavar="TRIPLE",
        help="Swift target triple (default: arm64-apple-macos26)",
    )
    parser.add_argument(
        "--output-dir",
        required=True,
        metavar="PATH",
        help="Directory for output .md files (one per top-level type)",
    )
    parser.add_argument(
        "--rate-limit",
        type=float,
        default=0.15,
        metavar="SECONDS",
        help="Seconds to sleep between fetches (default: 0.15)",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Re-fetch even if <ClassName>.md already exists",
    )
    return parser.parse_args()


def process_framework(
    framework: str,
    sdk: str,
    target: str,
    output_dir: Path,
    rate_limit: float,
    force: bool,
) -> dict:
    """Process one framework. Returns counts: fetched, skipped, failed."""
    counts = {"fetched": 0, "skipped": 0, "failed": 0}

    with tempfile.TemporaryDirectory() as tmpdir_str:
        tmpdir = Path(tmpdir_str)
        print(f"[{framework}] extracting symbol graph…", flush=True)
        graph = extract_symbol_graph(framework, sdk, target, tmpdir)

    symbols = enumerate_top_level_types(graph)
    print(f"[{framework}] {len(symbols)} top-level types found", flush=True)

    for sym in symbols:
        path_components = sym.get("pathComponents", [])
        class_name = path_components[0]
        out_path = output_dir / f"{class_name}.md"

        if out_path.exists() and not force:
            counts["skipped"] += 1
            continue

        slug = "/".join(pc.lower() for pc in path_components)
        url = f"{BASE_URL}/{framework.lower()}/{slug}.json"

        try:
            data = fetch_json(url)
        except Exception as e:
            print(f"WARN {framework}/{class_name}: fetch error: {e}", file=sys.stderr)
            counts["failed"] += 1
            time.sleep(rate_limit)
            continue

        if data is None:
            # 404 — no published page for this symbol (normal)
            print(f"WARN {framework}/{class_name}: 404 (no published page)", file=sys.stderr)
            counts["skipped"] += 1
            time.sleep(rate_limit)
            continue

        try:
            markdown = render_page(data, framework, url)
        except Exception as e:
            print(f"WARN {framework}/{class_name}: render error: {e}", file=sys.stderr)
            counts["failed"] += 1
            time.sleep(rate_limit)
            continue

        out_path.write_text(markdown, encoding="utf-8")
        counts["fetched"] += 1
        time.sleep(rate_limit)

    return counts


def main() -> None:
    args = parse_args()
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    total = {"fetched": 0, "skipped": 0, "failed": 0}
    per_framework: dict[str, dict] = {}

    for framework in args.frameworks:
        counts = process_framework(
            framework=framework,
            sdk=args.sdk,
            target=args.target,
            output_dir=output_dir,
            rate_limit=args.rate_limit,
            force=args.force,
        )
        per_framework[framework] = counts
        for k in total:
            total[k] += counts[k]

    per_fw_str = ", ".join(f"{fw}={c['fetched']}f/{c['skipped']}s/{c['failed']}x"
                           for fw, c in per_framework.items())
    print(
        f"fetched={total['fetched']} skipped={total['skipped']} failed={total['failed']} "
        f"per_framework={{{per_fw_str}}}"
    )

    if total["fetched"] == 0 and total["skipped"] == 0:
        print("error: no symbols fetched or found — check framework names and SDK path", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
