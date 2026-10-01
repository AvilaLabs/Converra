#!/usr/bin/env python3
"""Check curated handbook membership, release version, and built local links."""
from __future__ import annotations

import argparse
from html.parser import HTMLParser
from pathlib import Path
import re
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]


class Page(HTMLParser):
    def __init__(self, text: str):
        super().__init__()
        self.ids: set[str] = set()
        self.links: list[str] = []
        self.feed(text)

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if values.get("id"):
            self.ids.add(values["id"])
        if tag == "a" and values.get("name"):
            self.ids.add(values["name"])
        for key in ("href", "src"):
            if values.get(key):
                self.links.append(values[key])


def check(book: Path) -> list[str]:
    problems = []
    guide = ROOT / "docs/guide"
    chapters = re.findall(r"\]\(([^)#]+\.md)\)", (guide / "SUMMARY.md").read_text())
    declared = {Path(name) for name in chapters}
    sources = {p.relative_to(guide) for p in guide.rglob("*.md") if p.name != "SUMMARY.md"}
    if declared != sources:
        problems.append(f"Summary/source mismatch: {sorted(str(p) for p in declared ^ sources)}")
    if len(declared) != len(chapters):
        problems.append("Summary contains a duplicate chapter")
    for chapter in declared:
        if not (book / chapter.with_suffix(".html")).is_file():
            problems.append(f"Chapter did not render: {chapter}")
    workspace = (ROOT / "Cargo.toml").read_text()
    version = re.search(r'^version = "([^"]+)"', workspace, re.MULTILINE).group(1)
    if f"**Converra {version}**" not in (guide / "index.md").read_text():
        problems.append(f"Handbook overview must name workspace version {version}")
    pages = {p.resolve(): Page(p.read_text()) for p in book.rglob("*.html")}
    if not pages:
        problems.append(f"No built HTML at {book}")
    for path, page in pages.items():
        for link in page.links:
            url = urlsplit(link)
            if url.scheme or url.netloc:
                if url.netloc == "github.com":
                    for prefix in (
                        "/AvilaLabs/Converra/blob/main/",
                        "/AvilaLabs/Converra/tree/main/",
                        "/AvilaLabs/Converra/edit/main/",
                    ):
                        if url.path.startswith(prefix):
                            source = ROOT / unquote(url.path.removeprefix(prefix))
                            if not source.exists():
                                problems.append(f"Missing repository source: {link}")
                continue
            name = unquote(url.path)
            if name.startswith("/"):
                # book.toml declares the handbook's public /docs/ prefix.
                target = book / name.removeprefix("/docs/").lstrip("/")
            else:
                target = path.parent / name if name else path
            if target.is_dir():
                target /= "index.html"
            target = target.resolve()
            label = f"{path.relative_to(book.resolve())}: {link}"
            if not target.is_relative_to(book.resolve()):
                problems.append(f"Link escapes handbook: {label}")
            elif not target.is_file():
                problems.append(f"Missing local target: {label}")
            elif url.fragment and target in pages and unquote(url.fragment) not in pages[target].ids:
                problems.append(f"Missing anchor: {label}")
    # Only curated chapters may be published. mdBook copies unlisted static
    # source files, so checking SUMMARY alone would miss accidental exposure.
    allowed = {p.with_suffix(".html") for p in declared}
    allowed |= {Path(n) for n in ("print.html", "toc.html", "404.html")}
    config = (ROOT / "book.toml").read_text()
    redirects = config.split("[output.html.redirect]", 1)[-1]
    allowed |= {Path(n) for n in re.findall(r'^"([^"]+\.html)"\s*=', redirects, re.MULTILINE)}
    for path in pages:
        if path.relative_to(book.resolve()) not in allowed:
            problems.append(f"Unlisted HTML published: {path.name}")
    return sorted(set(problems))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("book", nargs="?", type=Path, default=ROOT / "dist/docs")
    args = parser.parse_args()
    failures = check(args.book)
    if failures:
        raise SystemExit("\n".join(failures))
    print("Handbook chapters, version, local files, and anchors are valid.")
