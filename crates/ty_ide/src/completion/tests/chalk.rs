use super::completion_test_builder;

fn assert_typed_once(test: &super::CompletionTest<'_>, name: &str, expected_ty: &str) {
    let completions: Vec<_> = test
        .completions()
        .iter()
        .filter(|completion| completion.name == name)
        .collect();
    assert_eq!(completions.len(), 1, "expected one `{name}` completion");
    assert_eq!(
        completions[0]
            .ty
            .map(|ty| ty.display(test.db).to_string())
            .as_deref(),
        Some(expected_ty)
    );
}

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
fn underscore_completion_excludes_non_feature_members() {
    let test = completion_test_builder(
        "from dataclasses import InitVar\nfrom typing import ClassVar\nfrom chalk.features import _, features\n\n@features\nclass Base:\n    inherited: float\n\n@features\nclass User(Base):\n    email: str\n    age: int\n    internal: ClassVar[str]\n    setup: InitVar[bytes]\n\n    def helper(self) -> str:\n        return self.email\n\n    _.<CURSOR>\n",
    );
    let test = test.build();
    test.contains("email")
        .contains("age")
        .contains("inherited")
        .not_contains("helper")
        .not_contains("internal")
        .not_contains("setup")
        .not_contains("__init__")
        .not_contains("__match_args__")
        .not_contains("__dataclass_fields__")
        .not_contains("__dataclass_params__");
    assert_typed_once(&test, "email", "str");
    assert_typed_once(&test, "age", "int");
    assert_typed_once(&test, "inherited", "int | float");
}

#[test]
fn relationship_underscore_completion_only_includes_feature_fields() {
    let builder = completion_test_builder(
        "from typing import ClassVar\nfrom chalk.features import DataFrame, _, features\n\n@features\nclass TransactionBase:\n    merchant: str\n\n@features\nclass Transaction(TransactionBase):\n    amount: float\n    internal: ClassVar[int]\n\n    def helper(self) -> float:\n        return self.amount\n\n@features\nclass User:\n    transactions: DataFrame[Transaction]\n\n    _.transactions.where(*(_.<CURSOR>,))\n",
    );
    let test = builder.build();
    test.contains("amount")
        .contains("merchant")
        .not_contains("internal")
        .not_contains("helper")
        .not_contains("__init__");
    assert_typed_once(&test, "amount", "int | float");
    assert_typed_once(&test, "merchant", "str");
}

#[test]
fn underscore_feature_field_completion_preserves_prefix_filtering() {
    let builder = completion_test_builder(
        "from chalk.features import _, features\n\n@features\nclass User:\n    email: str\n    age: int\n\n    _.em<CURSOR>\n",
    );
    let test = builder.build();
    test.contains("email").not_contains("age");
}

#[test]
fn ordinary_feature_instance_completion_is_not_filtered() {
    let builder = completion_test_builder(
        "from typing import ClassVar\nfrom chalk.features import features\n\n@features\nclass User:\n    email: str\n    internal: ClassVar[str]\n\n    def helper(self) -> str:\n        return self.email\n\nUser().<CURSOR>\n",
    );
    let test = builder.build();
    test.contains("email")
        .contains("internal")
        .contains("helper");
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

#[test]
fn functions_module_members() {
    for source in [
        "import chalk.functions as F\nF.<CURSOR>",
        "from chalk import functions as F\nF.<CURSOR>",
    ] {
        let builder = completion_test_builder(source);
        let test = builder.build();
        test.contains("array_agg")
            .contains("if_then_else")
            .contains("max_by")
            .contains("min_by")
            .contains("url_extract_host")
            .contains("zip_with");
    }
}
