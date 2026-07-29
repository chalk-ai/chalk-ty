import difflib
import enum
import hashlib
import json
import math
import re
from collections import Counter
from typing import TypedDict

from chalk import online


class Color(enum.Enum):
    RED = "red"


class Payload(TypedDict):
    x: int


@online
def resolver() -> None:
    math.sqrt(4.0)
    math.sqrt(4)
    json.loads('{"x": 1}')
    re.search("x", "xyz")
    bool(re.search("x", "xyz"))
    pattern = re.compile("x")
    pattern.search("xyz")
    len([1])
    hashlib.sha256(b"x").hexdigest()
    hashlib.sha256(b"x").digest()
    difflib.SequenceMatcher(None, "a", "b").ratio()
    Color.RED.__eq__(Color.RED)
    payload: Payload = {"x": 1}
    bool(payload)

    math.gcd(4, 2)
    json.load(1)
    re.finditer("x", "xyz")
    counts: Counter[str] = Counter(["x"])
    len(counts)

    math.sqrt("x")
    json.loads(1)
    re.search(1, "abc")
