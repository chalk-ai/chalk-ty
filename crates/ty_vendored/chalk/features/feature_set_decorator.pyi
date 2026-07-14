from typing import Any, Callable, TypeVar, overload

_T = TypeVar("_T")

@overload
def features(cls: type[_T]) -> type[_T]: ...
@overload
def features(**kwargs: Any) -> Callable[[type[_T]], type[_T]]: ...
