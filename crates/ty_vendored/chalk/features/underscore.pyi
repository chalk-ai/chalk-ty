from datetime import datetime
from typing import Any, TypeVar, overload

_T = TypeVar("_T")

class Underscore:
    def __getattr__(self, name: str) -> Any: ...
    def __getitem__(self, key: Any) -> Underscore: ...

# `Resolved` inherits from `Underscore`, so this import must follow its declaration.
from ty_chalk_extensions import Resolved  # noqa: E402

class UnderscoreRoot(Underscore):
    @property
    def chalk_window(self) -> datetime: ...
    @property
    def chalk_now(self) -> datetime: ...
    @overload
    def if_then_else(
        self,
        condition: Resolved[bool],
        if_true: Resolved[_T],
        if_false: Resolved[_T],
    ) -> Resolved[_T]: ...
    @overload
    def if_then_else(
        self,
        condition: Resolved[bool],
        if_true: Resolved[_T],
        if_false: _T,
    ) -> Resolved[_T]: ...
    @overload
    def if_then_else(
        self,
        condition: Resolved[bool],
        if_true: _T,
        if_false: Resolved[_T],
    ) -> Resolved[_T]: ...
    @overload
    def if_then_else(
        self,
        condition: Resolved[bool],
        if_true: _T,
        if_false: _T,
    ) -> Resolved[_T]: ...

_: UnderscoreRoot
