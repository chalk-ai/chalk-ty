# Chalk contextual underscores

## Underscore namespace boundaries

An underscore normally resolves against the enclosing feature class. A relationship subscript
temporarily replaces that namespace with its row type, and a nested relationship subscript replaces
it again with the nested row type. Leaving the subscript restores the enclosing namespace.

```py
from datetime import datetime

import chalk.functions as F
from chalk.features import DataFrame, _, features, has_many
from chalk.features import _ as feature

@features
class TransactionNamespace:
    transaction_leaf: int

@features
class UserNamespace:
    user_leaf: int

@features
class Transaction:
    timestamp: datetime
    amount: float
    transaction_only: int
    shared: TransactionNamespace

@features
class Group:
    group_only: int
    transactions: DataFrame[Transaction]

@features
class User:
    id: int
    user_only: int
    shared: UserNamespace
    transactions: DataFrame[Transaction] = has_many(lambda: True)
    groups: DataFrame[Group] = has_many(lambda: True)

    # Class-body expressions use the enclosing namespace.
    _.user_only
    _.shared.user_leaf
    _.shared.transaction_leaf  # error: [unresolved-attribute] "Object of type `UserNamespace` has no attribute `transaction_leaf`"

    # Relationship slices use the row namespace, not the enclosing namespace. Aliases of the
    # Chalk underscore use the same contextual root.
    _.transactions[_.timestamp, _.amount]
    _.transactions[feature.timestamp]
    _.transactions[_.user_only]  # error: [unresolved-attribute] "Object of type `Transaction` has no attribute `user_only`"
    _.transactions[_.shared.transaction_leaf]
    _.transactions[
        # error: [unresolved-attribute] "Object of type `TransactionNamespace` has no attribute `user_leaf`"
        _.shared.user_leaf
    ]

    # Nested relationship slices replace the outer row namespace with their own row namespace, then
    # restore the outer row namespace when the nested slice ends.
    _.groups[_.group_only, _.transactions[_.transaction_only]]
    _.groups[_.transactions[_.transaction_only], _.group_only]
    _.groups[
        _.transactions[
            _.group_only  # error: [unresolved-attribute] "Object of type `Transaction` has no attribute `group_only`"
        ]
    ]

    # Leaving the relationship subscript restores the enclosing namespace.
    _.user_only

    # Single-argument aggregations keep all contextual underscores inside the relationship slice.
    F.array_agg(_.transactions[_.amount])
```

## Row-dependent function arguments

`max_by` and `min_by` are different from ordinary calls: their ordering argument uses the row
namespace underlying the first argument. The selected value type does not become the namespace. In
the examples below the selection yields a `float`, but the second underscore should resolve against
`Transaction`.

Keep the function and method spellings together so changes can be checked against the complete
family.

```py
from datetime import datetime

import chalk.functions as F
from chalk.features import DataFrame, _, features, has_many
from chalk.functions import max_by as imported_max_by

@features
class Transaction:
    timestamp: datetime
    amount: float

@features
class Group:
    group_only: int
    transactions: DataFrame[Transaction]

@features
class User:
    id: int
    transactions: DataFrame[Transaction] = has_many(lambda: True)
    groups: DataFrame[Group] = has_many(lambda: True)

    # Function forms.
    F.max_by(
        _.transactions[_.amount],
        _.timestamp,
    )
    F.min_by(
        _.transactions[_.amount],
        _.timestamp,
    )
    F.max_by_n(
        _.transactions[_.amount],
        _.timestamp,
        3,
    )
    F.min_by_n(
        _.transactions[_.amount],
        _.timestamp,
        3,
    )
    imported_max_by(
        _.transactions[_.amount],
        _.timestamp,
    )
    F.max_by(
        _.transactions[_.amount > 0, _.amount],
        _.timestamp,
    )

    # Method forms.
    _.transactions[_.amount].max_by(_.timestamp)
    _.transactions[_.amount].min_by(_.timestamp)
    _.transactions[_.amount].max_by_n(
        _.timestamp,
        3,
    )
    _.transactions[_.amount].min_by_n(
        _.timestamp,
        3,
    )

    # Explicit feature paths avoid contextual underscore resolution and work today.
    F.max_by(_.transactions[_.amount], Transaction.timestamp)
    _.transactions[_.amount].max_by(Transaction.timestamp)

    # A parent-only feature must not satisfy a lookup in the ordering expression.
    F.max_by(_.transactions[_.amount], _.id)  # error: [unresolved-attribute] "Object of type `Transaction` has no attribute `id`"

    # A dependent argument inside another relationship scope should use the first argument's row,
    # then restore the surrounding relationship row when the call ends.
    _.groups[
        F.max_by(
            _.transactions[_.amount],
            _.timestamp,
        ),
        _.group_only,
    ]

    # The dependent argument scope must not leak into later expressions.
    _.id

# Explicit relationship receivers establish a row namespace even outside a feature class.
F.max_by(User.transactions[_.amount], _.timestamp)
```

## Dependent argument context and ordinary calls

The ordering argument can contain arbitrary expressions, including nested calls and subscripts. The
count argument and sibling expressions retain the surrounding namespace. Colliding field names with
distinct types make an incorrect root observable even when neither lookup reports an error.

```py
from datetime import datetime
from typing import Any

import chalk.functions as F
from chalk.features import DataFrame, _, features
from chalk.features import _ as feature

@features
class Detail:
    shared: bytes

@features
class Transaction:
    date: datetime
    datetime_6_months_ago: datetime
    amount: float
    shared: str
    optional: str | None
    details: DataFrame[Detail]

@features
class Account:
    transactions: DataFrame[Transaction]

def ordinary(first: Any, second: Any) -> Any: ...

@features
class User:
    shared: int
    account: Account | None
    transactions: DataFrame[Transaction]

    # The ticket expression, including its filter and declared result annotation.
    latest: float | None = F.max_by(_.transactions[_.date >= _.datetime_6_months_ago, _.amount], _.date)
    F.max_by(_.transactions[_.amount], reveal_type(_.shared))  # revealed: Resolved[str]
    _.transactions[_.amount].max_by(reveal_type(feature.shared))  # revealed: Resolved[str]
    # The stub independently reports that `n` must be a plain integer; its expression still uses
    # the surrounding namespace rather than the row namespace.
    F.max_by_n(
        _.transactions[_.amount],
        reveal_type(_.shared),  # revealed: Resolved[str]
        # error: [invalid-argument-type]
        reveal_type(_.shared),  # revealed: Resolved[int]
    )
    _.transactions[_.amount].min_by_n(
        reveal_type(_.shared),  # revealed: Resolved[str]
        # error: [invalid-argument-type]
        reveal_type(_.shared),  # revealed: Resolved[int]
    )
    ordinary(
        F.min_by(_.transactions[_.amount], _.date),
        reveal_type(_.shared),  # revealed: Resolved[int]
    )
    ordinary(_.transactions[_.amount], reveal_type(_.shared))  # revealed: Resolved[int]

    # Nested dependent calls switch to Detail and restore Transaction for the next expression.
    F.max_by(
        _.transactions[_.amount],
        ordinary(
            F.min_by(_.details[_.shared], reveal_type(_.shared)),  # revealed: Resolved[bytes]
            reveal_type(_.shared),  # revealed: Resolved[str]
        ),
    )
    # Existing conditional narrowing must compose with the dependent row context.
    F.max_by(
        _.transactions[_.amount],
        reveal_type(F.if_then_else(_.optional != None, _.optional, "fallback")),  # revealed: Resolved[str]
    )
    # Filtered receiver chains still describe Transaction rows.
    F.min_by(_.transactions[_.amount > 0][_.amount], reveal_type(_.shared))  # revealed: Resolved[str]
    F.max_by(_.transactions[_.amount], reveal_type(_.chalk_now))  # revealed: datetime
    F.max_by(_.account.transactions[_.amount], reveal_type(_.shared))  # revealed: Resolved[str]
    F.max_by(
        _.transactions[_.amount],
        _.missing,  # error: [unresolved-attribute] "Object of type `Transaction` has no attribute `missing`"
    )
    reveal_type(_.shared)  # revealed: Resolved[int]

# Module scope must get real row types and missing-member diagnostics, not silently use Any.
F.max_by(User.transactions[_.amount], reveal_type(_.shared))  # revealed: Resolved[str]
User.transactions[_.amount].min_by(reveal_type(_.shared))  # revealed: Resolved[str]
F.max_by(
    User.transactions[_.amount],
    _.missing,  # error: [unresolved-attribute] "Object of type `Transaction` has no attribute `missing`"
)

def in_resolver() -> None:
    F.max_by(User.transactions[_.amount], reveal_type(_.shared))  # revealed: Resolved[str]

def shadowed_underscore() -> None:
    class Local:
        shared: bytes

    _ = Local()
    F.max_by(User.transactions[feature.amount], reveal_type(_.shared))  # revealed: bytes

# Functions with the same spelling are ordinary Python calls.
def max_by(first: Any, second: Any) -> Any: ...

class Methods:
    def max_by(self, order: Any) -> Any: ...

@features
class Shadowed:
    shared: int
    transactions: DataFrame[Transaction]

    max_by(_.transactions[_.amount], reveal_type(_.shared))  # revealed: Resolved[int]
    Methods().max_by(reveal_type(_.shared))  # revealed: Resolved[int]
```

## Dependent call identity and conservative fallback

Re-exported functions retain their identity. Context recovery only identifies the current argument's
semantic role and a statically recoverable row source; it does not decide whether the whole call is
legal. An unavailable receiver or unpacked row source must not guess a row from unrelated
relationships. These unsupported forms retain the surrounding underscore context.

`reexports.py`:

```py
from chalk.functions import max_by as latest
```

`main.py`:

```py
from typing import Any

import chalk.functions as F
from chalk.features import DataFrame, _, features
from reexports import latest

@features
class Transaction:
    amount: float
    shared: str

class Ordinary:
    def __getitem__(self, index: Any) -> int:
        return 0

@features
class User:
    shared: int
    transactions: DataFrame[Transaction]
    unknown: Any

    latest(_.transactions[_.amount], reveal_type(_.shared))  # revealed: Resolved[str]
    alias = F.min_by
    alias(_.transactions[_.amount], reveal_type(_.shared))  # revealed: Resolved[str]

    # A non-relationship receiver and a dynamic receiver cannot establish a known row.
    F.max_by(Ordinary()[0], reveal_type(_.shared))  # revealed: Resolved[int]
    F.max_by(_.unknown[0], reveal_type(_.shared))  # revealed: Resolved[int]

    # Call legality comes from the stubs. Unknown arguments do not receive a row context, while a
    # recognized `sort` argument still does even if another argument makes the call invalid.
    # error: [missing-argument]
    # error: [unknown-argument]
    F.max_by(_.transactions[_.amount], order=reveal_type(_.shared))  # revealed: Resolved[int]
    # error: [missing-argument]
    # error: [unknown-argument]
    F.max_by(*(_.transactions[_.amount],), order=reveal_type(_.shared))  # revealed: Resolved[int]
    F.max_by(
        _.transactions[_.amount],
        reveal_type(_.shared),  # revealed: Resolved[str]
        3,  # error: [too-many-positional-arguments]
    )
    _.transactions[_.amount].max_by(
        reveal_type(_.shared),  # revealed: Resolved[str]
        3,  # error: [too-many-positional-arguments]
    )

    selection = _.transactions[_.amount]
    F.max_by(selection, reveal_type(_.shared))  # revealed: Resolved[str]
```

## Row-scoped filters and grouped aggregations

Call arguments that describe relationship rows use the relationship's row namespace. Every
positional `where` filter, `group_by` key, and `agg` expression uses that namespace. Leaving an
argument restores the surrounding namespace. The ordinary call binder, using the vendored Chalk
stubs, is responsible for rejecting unsupported argument forms.

```py
from typing import Any

from chalk.features import DataFrame, _, features

@features
class Transaction:
    amount: float
    category: str
    weight: int
    transaction_only: int
    shared: str

@features
class Group:
    shared: bytes
    transactions: DataFrame[Transaction]

class Ordinary:
    def where(self, *filters: Any, **kwargs: Any) -> Any:
        return self

    def group_by(self, *keys: Any, **kwargs: Any) -> Any:
        return self

    def agg(self, *expressions: Any, **kwargs: Any) -> Any: ...

@features
class User:
    user_only: int
    shared: int
    transactions: DataFrame[Transaction]
    groups: DataFrame[Group]

    # `where` continues a relationship filter expression. Every filter argument sees Transaction,
    # including later filters and chained calls.
    _.transactions[_.amount > 0].where(
        _.transaction_only > 0,
        reveal_type(_.shared) != "",  # revealed: Resolved[str]
    )
    _.transactions[_.amount > 0].where(
        _.transaction_only > 0,
    ).where(
        reveal_type(_.shared) != "",  # revealed: Resolved[str]
    )
    _.transactions[_.amount > 0].where(
        _.user_only > 0,  # error: [unresolved-attribute] "Object of type `Transaction` has no attribute `user_only`"
    )
    _.transactions.where(
        reveal_type(_.shared) != "",  # revealed: Resolved[str]
    )

    # All grouping keys and aggregation expressions see Transaction.
    _.transactions.group_by(
        _.category,
        reveal_type(_.shared),  # revealed: Resolved[str]
    ).agg(
        reveal_type(_.amount).sum(),  # revealed: Resolved[int | float]
        reveal_type(_.weight).sum(),  # revealed: Resolved[int]
    )
    _.transactions[_.amount > 0].group_by(_.category).agg(
        _.user_only,  # error: [unresolved-attribute] "Object of type `Transaction` has no attribute `user_only`"
    )
    _.transactions.group_by(_.category).where(
        reveal_type(_.shared) != "",  # revealed: Resolved[str]
    ).agg(
        reveal_type(_.amount)  # revealed: Resolved[int | float]
        .sum()
        .where(
            reveal_type(_.shared) != "",  # revealed: Resolved[str]
        ),
        reveal_type(_.weight),  # revealed: Resolved[int]
    )

    # An ordinary method with the same spelling does not establish a row namespace.
    Ordinary().where(reveal_type(_.shared))  # revealed: Resolved[int]
    Ordinary().group_by(reveal_type(_.shared))  # revealed: Resolved[int]
    Ordinary().agg(reveal_type(_.shared))  # revealed: Resolved[int]

    # Unsupported keyword forms retain the surrounding namespace.
    _.transactions.where()  # error: [missing-argument]
    _.transactions.group_by()  # error: [missing-argument]
    _.transactions.group_by(_.category).agg()  # error: [missing-argument]
    # error: [positional-only-parameter-as-kwarg]
    _.transactions[_.amount].where(filter=reveal_type(_.shared))  # revealed: Resolved[int]
    # error: [positional-only-parameter-as-kwarg]
    _.transactions.group_by(key=reveal_type(_.shared))  # revealed: Resolved[int]
    # error: [missing-argument]
    # error: [unknown-argument]
    _.transactions.group_by(_.category).agg(value=reveal_type(_.shared))  # revealed: Resolved[int]

    # The row scope does not leak past the call.
    reveal_type(_.shared)  # revealed: Resolved[int]
```

## Grouped aggregation nesting and namespace restoration

Nested grouped aggregations replace the surrounding row namespace and restore it at every boundary.

```py
from chalk.features import DataFrame, _, features

@features
class Transaction:
    amount: float
    category: str
    shared: str

@features
class Group:
    shared: bytes
    transactions: DataFrame[Transaction]

@features
class User:
    shared: int
    groups: DataFrame[Group]

    _.groups[
        _.transactions.group_by(
            reveal_type(_.shared),  # revealed: Resolved[str]
        ).agg(
            reveal_type(_.amount).sum(),  # revealed: Resolved[int | float]
        ),
        reveal_type(_.shared),  # revealed: Resolved[bytes]
    ]
    reveal_type(_.shared)  # revealed: Resolved[int]
```

## Approximate top-k row namespaces

`approx_top_k` can be called on a selected relationship column or inside a grouped aggregation. Its
`by` expression uses the selected relationship's row namespace. Its constant configuration arguments
do not receive that row namespace.

```py
from chalk.features import DataFrame, _, features

@features
class Transaction:
    merchant: str
    category: str
    weight: int
    transaction_only: int

@features
class User:
    user_only: int
    transactions: DataFrame[Transaction]

    _.transactions[_.merchant].approx_top_k(
        k=3,
        by=reveal_type(_.weight),  # revealed: Resolved[int]
        counters=6,
        return_total_weight=True,
    )
    # Only `by` uses the Transaction row namespace. Configuration arguments retain User.
    _.transactions[_.merchant].approx_top_k(
        # error: [invalid-argument-type]
        k=reveal_type(_.user_only),  # revealed: Resolved[int]
        by=reveal_type(_.weight),  # revealed: Resolved[int]
        # error: [invalid-argument-type]
        counters=reveal_type(_.user_only),  # revealed: Resolved[int]
        # error: [invalid-argument-type]
        return_total_weight=reveal_type(_.user_only),  # revealed: Resolved[int]
    )
    # `by` is optional and omitting it does not establish or leak another namespace.
    _.transactions[_.merchant].approx_top_k(k=3)
    reveal_type(_.user_only)  # revealed: Resolved[int]
    _.transactions[_.merchant].approx_top_k()  # error: [missing-argument]
    _.transactions.group_by(_.category).agg(
        _.merchant.approx_top_k(
            return_total_weight=True,
            by=reveal_type(_.weight),  # revealed: Resolved[int]
            counters=6,
            k=3,
        )
    )
    _.transactions.group_by(_.category).agg(
        _.merchant.approx_top_k(
            k=3,
            by=_.user_only,  # error: [unresolved-attribute] "Object of type `Transaction` has no attribute `user_only`"
        )
    )

# Explicit receivers also establish their row namespace outside a feature class.
User.transactions.where(reveal_type(_.transaction_only) > 0)  # revealed: Resolved[int]
User.transactions.group_by(reveal_type(_.category)).agg(  # revealed: Resolved[str]
    reveal_type(_.weight).sum(),  # revealed: Resolved[int]
)
User.transactions[_.merchant].approx_top_k(
    k=3,
    by=reveal_type(_.weight),  # revealed: Resolved[int]
)
```

## Keyword-bound row-dependent arguments

Valid keyword spellings use the same dependent row namespace without relying on source-order
argument indices. Constant arguments and invalid parameter names retain the surrounding namespace.

```py
import chalk.functions as F
from chalk.features import DataFrame, _, features

@features
class Transaction:
    amount: float
    shared: str

@features
class User:
    shared: int
    transactions: DataFrame[Transaction]

    F.max_by(
        sort=reveal_type(_.shared),  # revealed: Resolved[str]
        dataframe=_.transactions[_.amount],
    )
    F.max_by_n(
        # error: [invalid-argument-type]
        n=reveal_type(_.shared),  # revealed: Resolved[int]
        sort=reveal_type(_.shared),  # revealed: Resolved[str]
        dataframe=_.transactions[_.amount],
    )
    # Method aggregate operands are positional-only. The semantic hook still recognizes the `sort`
    # role independently of the call binder's diagnostic.
    # error: [positional-only-parameter-as-kwarg]
    _.transactions[_.amount].min_by(sort=reveal_type(_.shared))  # revealed: Resolved[str]

    # `order` is not a parameter of these Chalk functions.
    # error: [missing-argument]
    # error: [unknown-argument]
    F.max_by(_.transactions[_.amount], order=reveal_type(_.shared))  # revealed: Resolved[int]
```
