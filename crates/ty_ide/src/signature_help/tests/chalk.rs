use super::cursor_test;

#[test]
fn function_and_method_signatures() {
    let function = cursor_test(
        r#"
        import chalk.functions as F
        from chalk.features import DataFrame, _, features

        @features
        class Transaction:
            amount: float
            category: str

        @features
        class User:
            transactions: DataFrame[Transaction]

            selected = F.max_by(_.transactions[_.category], <CURSOR>)
        "#,
    );
    let result = function.signature_help().expect("Chalk function signature");
    let signature = &result.signatures[0];
    assert!(signature.label.contains("dataframe:"));
    assert!(signature.label.contains("sort:"));
    assert!(signature.label.contains("Resolved[_T]"));
    assert_eq!(signature.active_parameter, Some(1));

    let method = cursor_test(
        r#"
        from chalk.features import DataFrame, _, features

        @features
        class Transaction:
            amount: float
            category: str

        @features
        class User:
            transactions: DataFrame[Transaction]

            selected = _.transactions[_.category].max_by_n(_.amount, <CURSOR>)
        "#,
    );
    let result = method.signature_help().expect("Chalk method signature");
    let signature = &result.signatures[0];
    assert!(signature.label.contains("sort:"));
    assert!(signature.label.contains("n: int"));
    assert!(signature.label.contains("Resolved[list[_T]]"));
    assert_eq!(signature.active_parameter, Some(1));
}
