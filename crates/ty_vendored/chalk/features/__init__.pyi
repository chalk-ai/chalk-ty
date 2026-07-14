from typing import Any, Callable, Generic, TypeVar

from .feature_set_decorator import features as features
from .primary import Primary as Primary
from .underscore import (
    Underscore as Underscore,
    UnderscoreRoot as UnderscoreRoot,
    _ as _,
)

_T = TypeVar("_T")

class DataFrame(Generic[_T]):
    def __getattr__(self, name: str) -> Any: ...
    def __getitem__(self, key: Any) -> Any: ...

class Features(Generic[_T]): ...

def has_many(join: Callable[[], Any]) -> Any: ...
def __getattr__(name: str) -> Any: ...
