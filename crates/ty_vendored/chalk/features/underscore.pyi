from datetime import datetime

# BEGIN GENERATED CHALK UNDERSCORE IMPORTS
from typing import Any, Sequence, TypeVar, overload

from chalkdf.dataframe import ProjectedDataFrame

_T = TypeVar("_T")
_Row = TypeVar("_Row")
# END GENERATED CHALK UNDERSCORE IMPORTS

class Underscore:
    def __getattr__(self, name: str) -> Any: ...
    def __getitem__(self, key: Any) -> Underscore: ...
    # BEGIN GENERATED CHALK UNDERSCORE METHODS
    def max_by(
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
    def is_not_null(self) -> Resolved[bool]: ...
    # END GENERATED CHALK UNDERSCORE METHODS
    def where(self, filter: Any, /, *filters: Any) -> Underscore: ...
    def group_by(self, key: Any, /, *keys: Any) -> Underscore: ...
    def agg(self, expression: Any, /, *expressions: Any) -> Any: ...
    def approx_top_k(
        self,
        *,
        k: int,
        by: Any = ...,
        counters: int = ...,
        return_total_weight: bool = ...,
    ) -> Any: ...

# `Resolved` inherits from `Underscore`, so this import must follow its declaration.
from ty_chalk_extensions import Resolved  # noqa: E402

class UnderscoreRoot(Underscore):
    @property
    def chalk_window(self) -> datetime: ...
    @property
    def chalk_now(self) -> datetime: ...
    def if_then_else(
        self,
        condition: Resolved[bool | None],
        if_true: _T | Resolved[_T],
        if_false: _T | Resolved[_T],
    ) -> Resolved[_T]: ...

_: UnderscoreRoot
