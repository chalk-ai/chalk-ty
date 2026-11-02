# Generated into chalk.functions by generate_chalk_functions_stub.py.
# Edit this template, then regenerate the vendored stub.

from typing import Any, Generic, Sequence, TypeVar, overload

from chalkdf.dataframe import ProjectedDataFrame
from ty_chalk_extensions import Resolved
from typing_extensions import Never

_T = TypeVar("_T")
_Row = TypeVar("_Row")
_U = TypeVar("_U")

def if_then_else(
    condition: Resolved[bool | None],
    if_true: _T | Resolved[_T],
    if_false: _T | Resolved[_T],
) -> Resolved[_T]: ...

# Explicit declarations let semantic inference recognize row-dependent calls through aliases.
def max_by(
    dataframe: Resolved[ProjectedDataFrame[_Row, _T]], sort: Any
) -> Resolved[_T]: ...
def min_by(
    dataframe: Resolved[ProjectedDataFrame[_Row, _T]], sort: Any
) -> Resolved[_T]: ...
def max_by_n(
    dataframe: Resolved[ProjectedDataFrame[_Row, _T]], sort: Any, n: int
) -> Resolved[list[_T]]: ...
def min_by_n(
    dataframe: Resolved[ProjectedDataFrame[_Row, _T]], sort: Any, n: int
) -> Resolved[list[_T]]: ...
def array_agg(expr: Resolved[ProjectedDataFrame[_Row, _T]]) -> Resolved[list[_T]]: ...
def set_agg(expr: Resolved[ProjectedDataFrame[_Row, _T]]) -> Resolved[list[_T]]: ...
@overload
def array_sum(expr: Resolved[Sequence[int]] | Sequence[int]) -> Resolved[int]: ...
@overload
def array_sum(
    expr: Resolved[Sequence[int | float]] | Sequence[int | float],
) -> Resolved[float]: ...
def array_average(
    expr: Resolved[Sequence[int | float]] | Sequence[int | float],
) -> Resolved[float]: ...
@overload
def coalesce(
    value: Resolved[_T | None],
    fallback: _T | Resolved[_T],
    /,
    *values: _T | Resolved[_T | None],
) -> Resolved[_T]: ...
@overload
def coalesce(*vals: _T | Resolved[_T]) -> Resolved[_T]: ...
def recover(*vals: _T | Resolved[_T]) -> Resolved[_T]: ...
def is_null(expr: Any) -> Resolved[bool]: ...
def is_not_null(expr: Any) -> Resolved[bool]: ...

class When(Generic[_T]):
    def then(self, value: _U | Resolved[_U]) -> Then[_T | _U]: ...

class Then(Generic[_T]):
    def when(self, condition: bool | Resolved[bool | None]) -> When[_T]: ...
    def otherwise(self, default: _U | Resolved[_U]) -> Resolved[_T | _U]: ...

def when(condition: bool | Resolved[bool | None]) -> When[Never]: ...
