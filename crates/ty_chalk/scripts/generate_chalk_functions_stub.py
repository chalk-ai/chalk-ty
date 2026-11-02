#!/usr/bin/env python3
"""Generate precise Chalk declarations and public functions from a name manifest."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

BEGIN = "# BEGIN GENERATED CHALK FUNCTIONS"
END = "# END GENERATED CHALK FUNCTIONS"
FALLBACK = "def __getattr__(name: str) -> Any: ..."
PRECISE = Path(__file__).with_name("chalk_functions_precise.pyi")
SUPPORTING_BLOCKS = {
    "chalk/features/underscore.pyi": {
        "UNDERSCORE IMPORTS": """from typing import Any, Sequence, TypeVar, overload

from chalkdf.dataframe import ProjectedDataFrame

_T = TypeVar("_T")
_Row = TypeVar("_Row")""",
        "UNDERSCORE METHODS": """    def max_by(
        self: Resolved[ProjectedDataFrame[_Row, _T]], sort: Any, /
    ) -> Resolved[_T]: ...
    def min_by(
        self: Resolved[ProjectedDataFrame[_Row, _T]], sort: Any, /
    ) -> Resolved[_T]: ...
    def max_by_n(
        self: Resolved[ProjectedDataFrame[_Row, _T]], sort: Any, n: int, /
    ) -> Resolved[list[_T]]: ...
    def min_by_n(
        self: Resolved[ProjectedDataFrame[_Row, _T]], sort: Any, n: int, /
    ) -> Resolved[list[_T]]: ...
    def array_agg(
        self: Resolved[ProjectedDataFrame[_Row, _T]],
    ) -> Resolved[list[_T]]: ...
    def set_agg(self: Resolved[ProjectedDataFrame[_Row, _T]]) -> Resolved[list[_T]]: ...
    @overload
    def array_sum(self: Resolved[Sequence[int]]) -> Resolved[int]: ...
    @overload
    def array_sum(self: Resolved[Sequence[int | float]]) -> Resolved[float]: ...
    def array_average(self: Resolved[Sequence[int | float]]) -> Resolved[float]: ...
    def coalesce(
        self: Resolved[_T | None],
        fallback: _T | Resolved[_T],
        /,
        *values: _T | Resolved[_T | None],
    ) -> Resolved[_T]: ...
    def recover(self: Resolved[_T], *values: _T | Resolved[_T]) -> Resolved[_T]: ...
    def is_null(self) -> Resolved[bool]: ...
    def is_not_null(self) -> Resolved[bool]: ...""",
    },
    "chalkdf/dataframe.pyi": {
        "DATAFRAME IMPORTS": """from typing import Any, Generic, TypeVar

from typing_extensions import TypeVarTuple, Unpack

_Columns = TypeVarTuple("_Columns")
_Row = TypeVar("_Row")
_Selection = TypeVar("_Selection")""",
        "PROJECTED DATAFRAME": """class ProjectedDataFrame(DataFrame[_Row], Generic[_Row, _Selection]): ...""",
    },
    "ty_chalk_extensions/__init__.pyi": {
        "RESOLVED IMPORTS": """from typing import Any, Generic, TypeVar, overload

from chalk.features.underscore import Underscore
from chalk.streams import Windowed as _ChalkWindowed
from chalkdf.dataframe import DataFrame, ProjectedDataFrame

_T_co = TypeVar("_T_co", covariant=True)
_Row = TypeVar("_Row")
_Selection = TypeVar("_Selection")""",
        "RESOLVED GETITEM": """    @overload
    def __getitem__(
        self: Resolved[DataFrame[_Row]], key: Resolved[bool]
    ) -> Resolved[DataFrame[_Row]]: ...
    @overload
    def __getitem__(
        self: Resolved[DataFrame[_Row]], key: Resolved[_Selection]
    ) -> Resolved[ProjectedDataFrame[_Row, _Selection]]: ...
    @overload
    def __getitem__(
        self: Resolved[DataFrame[_Row]],
        key: tuple[Resolved[bool], Resolved[_Selection]],
    ) -> Resolved[ProjectedDataFrame[_Row, _Selection]]: ...
    @overload
    def __getitem__(self, key: Any) -> Resolved[_T_co]: ...""",
    },
}


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


def precise_declarations(precise: str) -> set[str]:
    return set(re.findall(r"^def (\w+)\(", precise, re.MULTILINE))


def generated_block(names: list[str], precise: set[str]) -> str:
    declarations = [
        f"def {name}(*args: Any, **kwargs: Any) -> Any: ..."
        for name in names
        if name not in precise
    ]
    return "\n".join([BEGIN, *declarations, "", END])


def replace_block(stub: str, name: str, declarations: str) -> str:
    indent = "    " if name in {"UNDERSCORE METHODS", "RESOLVED GETITEM"} else ""
    begin = f"{indent}# BEGIN GENERATED CHALK {name}\n"
    end = f"{indent}# END GENERATED CHALK {name}"
    if stub.count(begin) != 1 or stub.count(end) != 1:
        raise ValueError(f"expected exactly one generated Chalk {name} block")
    start = stub.index(begin) + len(begin)
    stop = stub.index(end, start)
    separator = "\n\n" if name == "PROJECTED DATAFRAME" else "\n"
    return stub[:start] + declarations + separator + stub[stop:]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("-o", "--output", type=Path)
    parser.add_argument(
        "--vendored-root", type=Path, help="Update all Chalk stubs under this directory"
    )
    args = parser.parse_args()
    if args.output is not None and args.vendored_root is not None:
        parser.error("--output and --vendored-root cannot be used together")

    names = load_manifest(args.manifest)
    precise = PRECISE.read_text()
    declared = precise_declarations(precise)
    if missing := declared.difference(names):
        raise ValueError(
            f"precise Chalk functions missing from {args.manifest}: {sorted(missing)}"
        )
    generated = (
        precise + "\n" + generated_block(names, declared) + "\n\n" + FALLBACK + "\n"
    )
    if args.vendored_root is not None:
        updated = {}
        for relative_path, blocks in SUPPORTING_BLOCKS.items():
            path = args.vendored_root / relative_path
            stub = path.read_text()
            for name, declarations in blocks.items():
                stub = replace_block(stub, name, declarations)
            updated[path] = stub
        updated[args.vendored_root / "chalk/functions/__init__.pyi"] = generated
        for path, stub in updated.items():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(stub)
    elif args.output is None:
        print(generated, end="")
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(generated)


if __name__ == "__main__":
    main()
