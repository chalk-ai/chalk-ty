from typing import Any, Generic

from typing_extensions import TypeVarTuple, Unpack

_Columns = TypeVarTuple("_Columns")

class DataFrame(Generic[Unpack[_Columns]]):
    def __getattr__(self, name: str) -> Any: ...
    def __getitem__(self, key: Any) -> Any: ...
