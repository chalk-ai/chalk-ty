use super::completion_test_builder;

#[test]
fn underscore_uses_contextual_feature_class() {
    for receiver in ["_", "feature"] {
        let source = format!(
            "from chalk.features import _, features\nfrom chalk.features import _ as feature\n\n@features\nclass User:\n    email: str\n    age: int\n\n    {receiver}.<CURSOR>\n"
        );
        let builder = completion_test_builder(&source);
        let test = builder.build();
        test.contains("email")
            .contains("age")
            .not_contains("where")
            .not_contains("group_by");
    }
}

#[test]
fn underscore_completion_requires_contextual_receiver() {
    let test = completion_test_builder("from chalk.features import _\n\n_.<CURSOR>\n");
    test.build().contains("where");

    let test = completion_test_builder(
        "from chalk.features import _, features\n\nclass Local:\n    local: int\n\n@features\nclass User:\n    email: str\n    _ = Local()\n    _.<CURSOR>\n",
    );
    test.build()
        .contains("local")
        .not_contains("email")
        .not_contains("where");
}

#[test]
fn underscore_completion_uses_active_relationship_root() {
    let test = completion_test_builder(
        "from chalk.features import DataFrame, _, features, has_many\n\n@features\nclass Transaction:\n    amount: float\n\n@features\nclass User:\n    user_only: int\n    transactions: DataFrame[Transaction] = has_many(lambda: True)\n\n    _.transactions[_.<CURSOR>]\n",
    );
    test.build().contains("amount").not_contains("user_only");
}

#[test]
fn underscore_completion_uses_relationship_root_in_starred_arguments() {
    for expression in [
        "_.transactions.where(*(_.<CURSOR>,))",
        "_.transactions.group_by(*(_.<CURSOR>,))",
        "_.transactions.group_by(*(_.category,)).agg(*(_.<CURSOR>,))",
    ] {
        let source = format!(
            "from chalk.features import DataFrame, _, features\n\n@features\nclass Transaction:\n    amount: float\n    category: str\n\n@features\nclass User:\n    user_only: int\n    transactions: DataFrame[Transaction]\n\n    {expression}\n"
        );
        completion_test_builder(&source)
            .build()
            .contains("amount")
            .contains("category")
            .not_contains("user_only");
    }
}

#[test]
fn starred_argument_relationship_root_does_not_leak() {
    let test = completion_test_builder(
        "from typing import Any\nfrom chalk.features import DataFrame, _, features\n\n@features\nclass Transaction:\n    amount: float\n\nclass Ordinary:\n    def where(self, *filters: Any) -> Any: ...\n\n@features\nclass User:\n    user_only: int\n    transactions: DataFrame[Transaction]\n\n    Ordinary().where(*(_.<CURSOR>,))\n",
    );
    test.build().contains("user_only").not_contains("amount");

    let test = completion_test_builder(
        "from chalk.features import DataFrame, _, features\n\n@features\nclass Transaction:\n    amount: float\n\n@features\nclass User:\n    user_only: int\n    transactions: DataFrame[Transaction]\n\n    (_.transactions.where(*(_.amount,)), _.<CURSOR>)\n",
    );
    test.build().contains("user_only").not_contains("amount");
}
