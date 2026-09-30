"""Stage offline release guides and embedded-data attribution beside binaries."""

from pathlib import Path
import shutil
import sys


def stage(destination: Path) -> None:
    root = Path(__file__).resolve().parent.parent
    destination.mkdir(parents=True, exist_ok=True)

    def copy(relative: str, target: str | None = None) -> None:
        output = destination / (target or relative)
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / relative, output)

    copy("packaging/README-release.md", "README.md")
    for relative in ["LICENSE", "CHANGELOG.md", "ROADMAP.md", "CONTRIBUTING.md",
                     "crates/optcoil-mcp/README.md", "crates/optcoil-py/README.md",
                     "packaging/README.md"]:
        copy(relative)
    shutil.copytree(root / "docs", destination / "docs", dirs_exist_ok=True)
    for name in ["first-study.json", "oc-007.json"]:
        copy(f"benchmarks/coupled/{name}")
    # Keep the original relative attribution paths, without bundling raw tables
    # and signed bundles already embedded in the executables.
    for source in sorted((root / "data/materials").rglob("*")):
        if source.is_file() and (source.suffix == ".md" or source.name == "material.json"
                                 or source.name.startswith("LICENSE")):
            copy(str(source.relative_to(root)))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: stage-guides.py <archive-staging-directory>")
    stage(Path(sys.argv[1]))
