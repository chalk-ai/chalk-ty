from typing import Any, TypeVar

from ty_chalk_extensions import Resolved

_T = TypeVar("_T")

def if_then_else(
    condition: Resolved[bool | None],
    if_true: _T | Resolved[_T],
    if_false: _T | Resolved[_T],
) -> Resolved[_T]: ...
def __getattr__(name: str) -> Any: ...
