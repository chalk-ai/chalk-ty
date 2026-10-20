#!/usr/bin/env python3
"""Generate public ``chalk.functions`` declarations from a name manifest."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

BEGIN = "# BEGIN GENERATED CHALK FUNCTIONS"
END = "# END GENERATED CHALK FUNCTIONS"
PRECISE_DECLARATIONS = frozenset(
    {"if_then_else", "max_by", "max_by_n", "min_by", "min_by_n"}
)


def load_manifest(path: Path) -> list[str]:
    names = [
        line
        for raw_line in path.read_text().splitlines()
        if (line := raw_line.strip()) and not line.startswith("#")
    ]
    if names != sorted(set(names)):
        raise ValueError(f"{path} must contain unique, sorted names")
    invalid = [name for name in names if not name.isidentifier()]
    if invalid:
        raise ValueError(f"{path} contains invalid identifiers: {invalid}")
    return names


def generated_block(names: list[str]) -> str:
    declarations = [
        f"def {name}(*args: Any, **kwargs: Any) -> Any: ..."
        for name in names
        if name not in PRECISE_DECLARATIONS
    ]
    return "\n".join([BEGIN, *declarations, "", END])


def replace_generated_block(stub: str, block: str) -> str:
    pattern = re.compile(rf"{re.escape(BEGIN)}.*?{re.escape(END)}", re.DOTALL)
    updated, replacements = pattern.subn(block, stub)
    if replacements != 1:
        raise ValueError("expected exactly one generated chalk.functions block")
    return updated


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("stub", type=Path)
    parser.add_argument("-o", "--output", type=Path)
    args = parser.parse_args()

    generated = replace_generated_block(
        args.stub.read_text(), generated_block(load_manifest(args.manifest))
    )
    if args.output is None:
        print(generated, end="")
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(generated)


if __name__ == "__main__":
    main()
