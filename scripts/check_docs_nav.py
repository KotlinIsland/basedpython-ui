"""check the docs tree against the zensical.toml nav

two lists are meant to agree: the pages on disk under `docs/`, and the document
paths in the `zensical.toml` nav. a page missing from the nav is orphaned in the
built site, and a nav entry with no page behind it is a dead link

`zensical build --strict` reports neither, so this is the only guard against the
two drifting apart. two more things are checked here because the site cannot
check them either: a link that leaves `docs/`, and a test cited by a name the
tree no longer has

run with `python3.14 scripts/check_docs_nav.py` (`tomllib` needs 3.11+)
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).parent.parent
DOCS = ROOT / "docs"

# the pages that may cite a test by name: the site, and the readme beside it
PAGES = [DOCS, ROOT / "README.md"]

# where a test is defined: `def test_...` in the basedpython tests, and `fn`
# names in the rust core's crate (its own tests included). a build directory
# under `native/` holds generated rust, which is not the tree
TESTS = ROOT / "tests"
NATIVE = ROOT / "native"


def nav_paths(nav: object) -> list[str]:
    """every document path in the nav, in nav order"""
    if isinstance(nav, str):
        return [nav]
    if isinstance(nav, list):
        return [path for entry in nav for path in nav_paths(entry)]
    if isinstance(nav, dict):
        return [path for value in nav.values() for path in nav_paths(value)]
    return []


# a test in this project is named as a sentence behind a `test_` prefix —
# `test_stable_children_are_skipped` — so a backticked name of that shape in the
# docs is a claim that such a test exists in the tree. a page that goes on
# naming a test after it was renamed is a page nobody can check, which is the
# failure this catches
SENTENCE = re.compile(r"^test_[a-z0-9_]+$")


def _source_files(root: Path, suffix: str) -> list[Path]:
    """every `suffix` file under `root`, skipping build output"""
    return [
        path
        for path in sorted(root.rglob(f"*{suffix}"))
        if "target" not in path.relative_to(root).parts
    ]


def _known_names() -> set[str]:
    """every test the tree defines, in the spelling docs use"""
    by_source = "\n".join(
        path.read_text(encoding="utf-8") for path in _source_files(TESTS, ".by")
    )
    rust_source = "\n".join(
        path.read_text(encoding="utf-8") for path in _source_files(NATIVE, ".rs")
    )
    names = set(re.findall(r"^\s*def (test_[a-z0-9_]*)\b", by_source, re.M))
    names |= set(re.findall(r"\bfn ([a-z_][a-z0-9_]*)", rust_source))
    return names


def _pages() -> list[Path]:
    """every page that may cite a test"""
    pages: list[Path] = []
    for entry in PAGES:
        if entry.is_dir():
            pages.extend(sorted(entry.rglob("*.md")))
        elif entry.is_file():
            pages.append(entry)
    return pages


def vanished_names() -> list[str]:
    """test names the docs cite that no longer exist in the tree"""
    known = _known_names()
    gone: dict[str, set[str]] = {}
    for page in _pages():
        for found in re.finditer(r"`([a-z][a-z0-9_]+)`", page.read_text(encoding="utf-8")):
            name = found.group(1)
            if SENTENCE.match(name) and name not in known:
                gone.setdefault(name, set()).add(str(page.relative_to(ROOT)))
    return [
        f"{name} — cited by {', '.join(sorted(where))}"
        for name, where in sorted(gone.items())
    ]


def escaping_links() -> list[str]:
    """every relative link that leaves `docs/`

    a docs page can only link to another docs page: the site is built from this
    directory, so `../../README.md` resolves to nothing and `zensical build
    --strict` fails on it. it is checked here, where it costs nothing, so the
    failure is found by the hook rather than by a build
    """
    escaping = []
    for page in sorted(DOCS.rglob("*.md")):
        for number, line in enumerate(
            page.read_text(encoding="utf-8").splitlines(), start=1
        ):
            for target in re.findall(r"\]\(([^)]+)\)", line):
                if target.startswith(("http://", "https://", "#", "mailto:")):
                    continue
                landing = (page.parent / target.split("#", 1)[0]).resolve()
                if not landing.is_relative_to(DOCS.resolve()):
                    here = page.relative_to(DOCS)
                    escaping.append(f"{here}:{number} -> {target}")
    return escaping


def main() -> int:
    config = tomllib.loads((ROOT / "zensical.toml").read_text(encoding="utf-8"))
    nav = nav_paths(config["project"]["nav"])

    on_disk = sorted(str(p.relative_to(DOCS)) for p in DOCS.rglob("*.md"))

    problems: list[str] = []

    def report(label: str, items: list[str]):
        if items:
            problems.append(f"{label}:\n" + "\n".join(f"  - {i}" for i in items))

    report("on disk but not in the zensical.toml nav", sorted(set(on_disk) - set(nav)))
    report(
        "in the zensical.toml nav but missing on disk",
        sorted(p for p in nav if not (DOCS / p).is_file()),
    )
    report(
        "duplicated in the zensical.toml nav",
        sorted({p for p in nav if nav.count(p) > 1}),
    )
    report(
        "linking out of docs/, which the site cannot resolve — quote the file "
        "or say its name instead of linking to it",
        escaping_links(),
    )
    report(
        "named as evidence in the docs and gone from the tree — a page that "
        "cites a test by a name nothing has is a page nobody can check",
        vanished_names(),
    )

    if problems:
        print("\n\n".join(problems), file=sys.stderr)
        print(
            f"\n{len(problems)} problem(s) — see {Path(__file__).name}", file=sys.stderr
        )
        return 1

    print(f"{len(on_disk)} pages: the nav and the disk agree")
    return 0


if __name__ == "__main__":
    sys.exit(main())
