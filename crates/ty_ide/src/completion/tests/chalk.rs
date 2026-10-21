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
