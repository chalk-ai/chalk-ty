use super::completion_test_builder;

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
