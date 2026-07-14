from typing import Any, TypeVar, overload

from ty_chalk_extensions import Resolved

_T = TypeVar("_T")

@overload
def if_then_else(
    condition: Resolved[bool],
    if_true: Resolved[_T],
    if_false: Resolved[_T],
) -> Resolved[_T]: ...
@overload
def if_then_else(
    condition: Resolved[bool],
    if_true: Resolved[_T],
    if_false: _T,
) -> Resolved[_T]: ...
@overload
def if_then_else(
    condition: Resolved[bool],
    if_true: _T,
    if_false: Resolved[_T],
) -> Resolved[_T]: ...
@overload
def if_then_else(
    condition: Resolved[bool],
    if_true: _T,
    if_false: _T,
) -> Resolved[_T]: ...
def __getattr__(name: str) -> Any: ...
