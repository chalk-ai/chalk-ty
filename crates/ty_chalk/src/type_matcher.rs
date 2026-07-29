use std::collections::{HashMap, HashSet};

use ty_python_semantic::chalk::{
    ChalkClassRelation, ChalkContainerKind, ChalkContainerType, ChalkIntersection,
    chalk_class_derived_from, chalk_container_type, chalk_exact_class_object,
    chalk_exact_instance_class, chalk_instance_derived_from, chalk_is_enum,
    chalk_is_logical_struct, chalk_module_is, chalk_type_shape,
};
use ty_python_semantic::types::Type;
use ty_python_semantic::{Db, chalk::ChalkTypeShape};

use crate::supported_functions::SupportedTy;

const DEFAULT_EXPANSION_LIMIT: usize = 256;

/// The result of matching a native ty type against a Chalk registry type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypeMatch {
    Match,
    NoMatch,
    Inconclusive,
}

impl TypeMatch {
    /// Aggregates requirements that must all match.
    ///
    /// A known mismatch dominates an inconclusive sibling.
    #[must_use]
    pub(crate) const fn all(self, other: Self) -> Self {
        match (self, other) {
            (Self::NoMatch, _) | (_, Self::NoMatch) => Self::NoMatch,
            (Self::Inconclusive, _) | (_, Self::Inconclusive) => Self::Inconclusive,
            (Self::Match, Self::Match) => Self::Match,
        }
    }

    /// Aggregates alternatives where any matching alternative is sufficient.
    #[must_use]
    const fn any(self, other: Self) -> Self {
        match (self, other) {
            (Self::Match, _) | (_, Self::Match) => Self::Match,
            (Self::Inconclusive, _) | (_, Self::Inconclusive) => Self::Inconclusive,
            (Self::NoMatch, Self::NoMatch) => Self::NoMatch,
        }
    }

    /// Aggregates an iterator of requirements that must all match.
    pub(crate) fn all_results(results: impl IntoIterator<Item = Self>) -> Self {
        results.into_iter().fold(Self::Match, TypeMatch::all)
    }

    /// Aggregates an iterator of alternatives where any matching alternative is sufficient.
    pub(crate) fn any_results(results: impl IntoIterator<Item = Self>) -> Self {
        results.into_iter().fold(Self::NoMatch, TypeMatch::any)
    }
}

/// Matches a native ty type directly against a Chalk registry type.
///
/// The match is transient: neither the native type graph nor intermediate expansions are retained
/// in a Salsa-cached value.
pub(crate) fn match_supported_type<'db>(
    db: &'db dyn Db,
    actual: Type<'db>,
    expected: &SupportedTy,
) -> TypeMatch {
    Matcher::new(db, DEFAULT_EXPANSION_LIMIT).match_type(actual, Expected::Registry(expected))
}

#[derive(Clone, Copy)]
enum Expected<'a> {
    Registry(&'a SupportedTy),
    JsonValue,
    JsonStringKey,
}

impl Expected<'_> {
    fn key(self) -> ExpectedKey {
        match self {
            Self::Registry(expected) => ExpectedKey::Registry(std::ptr::from_ref(expected)),
            Self::JsonValue => ExpectedKey::JsonValue,
            Self::JsonStringKey => ExpectedKey::JsonStringKey,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ExpectedKey {
    Registry(*const SupportedTy),
    JsonValue,
    JsonStringKey,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Pair<'db> {
    actual: Type<'db>,
    expected: ExpectedKey,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct MemoKey<'db> {
    pair: Pair<'db>,
    depth: usize,
}

struct Matcher<'db> {
    db: &'db dyn Db,
    expansion_limit: usize,
    active: HashSet<Pair<'db>>,
    memo: HashMap<MemoKey<'db>, TypeMatch>,
}

impl<'db> Matcher<'db> {
    fn new(db: &'db dyn Db, expansion_limit: usize) -> Self {
        Self {
            db,
            expansion_limit,
            active: HashSet::new(),
            memo: HashMap::new(),
        }
    }

    fn match_type(&mut self, actual: Type<'db>, expected: Expected<'_>) -> TypeMatch {
        self.match_type_at_depth(actual, expected, 0)
    }

    fn match_type_at_depth(
        &mut self,
        actual: Type<'db>,
        expected: Expected<'_>,
        depth: usize,
    ) -> TypeMatch {
        let pair = Pair {
            actual,
            expected: expected.key(),
        };
        let memo_key = MemoKey { pair, depth };
        if let Some(result) = self.memo.get(&memo_key) {
            return *result;
        }
        if !self.active.insert(pair) {
            return TypeMatch::Match;
        }

        let result = self.match_type_inner(actual, expected, depth);
        self.active.remove(&pair);
        self.memo.insert(memo_key, result);
        result
    }

    fn match_type_inner(
        &mut self,
        actual: Type<'db>,
        expected: Expected<'_>,
        depth: usize,
    ) -> TypeMatch {
        match chalk_type_shape(self.db, actual) {
            ChalkTypeShape::Dynamic | ChalkTypeShape::Unavailable => TypeMatch::Inconclusive,
            ChalkTypeShape::Never => TypeMatch::Match,
            ChalkTypeShape::Expanded(expanded) => self.descend(expanded, expected, depth),
            ChalkTypeShape::Union(elements) => {
                let Some(depth) = self.next_depth(depth) else {
                    return TypeMatch::Inconclusive;
                };
                let mut result = TypeMatch::Match;
                for element in elements {
                    result = result.all(self.match_type_at_depth(*element, expected, depth));
                }
                result
            }
            ChalkTypeShape::Intersection(intersection) => {
                self.match_intersection(actual, intersection, expected, depth)
            }
            ChalkTypeShape::Concrete => self.match_concrete(actual, expected, depth),
        }
    }

    fn match_intersection(
        &mut self,
        actual: Type<'db>,
        intersection: ChalkIntersection<'db>,
        expected: Expected<'_>,
        depth: usize,
    ) -> TypeMatch {
        let Some(depth) = self.next_depth(depth) else {
            return TypeMatch::Inconclusive;
        };

        for positive in intersection.positive {
            match self.match_type_at_depth(positive, expected, depth) {
                TypeMatch::Match => return TypeMatch::Match,
                TypeMatch::NoMatch | TypeMatch::Inconclusive => {}
            }
        }

        let top = intersection.top_materialization;
        let bottom = intersection.bottom_materialization;
        if top == actual || bottom == actual {
            return TypeMatch::Inconclusive;
        }
        let top_result = self.match_type_at_depth(top, expected, depth);
        let bottom_result = self.match_type_at_depth(bottom, expected, depth);
        if top_result == bottom_result {
            top_result
        } else {
            TypeMatch::Inconclusive
        }
    }

    fn match_concrete(
        &mut self,
        actual: Type<'db>,
        expected: Expected<'_>,
        depth: usize,
    ) -> TypeMatch {
        match expected {
            Expected::JsonValue => self.match_json_value(actual, depth),
            Expected::JsonStringKey => self.exact_instance(actual, "builtins", "str"),
            Expected::Registry(expected) => {
                if actual.is_none(self.db) {
                    return if expected.accepts_none() {
                        TypeMatch::Match
                    } else {
                        TypeMatch::NoMatch
                    };
                }
                if matches!(expected, SupportedTy::Any { .. }) {
                    return TypeMatch::Match;
                }
                self.match_registry_type(actual, expected, depth)
            }
        }
    }

    fn match_registry_type(
        &mut self,
        actual: Type<'db>,
        expected: &SupportedTy,
        depth: usize,
    ) -> TypeMatch {
        match expected {
            SupportedTy::Any { .. } => TypeMatch::Match,
            SupportedTy::Bool { .. } => self.exact_instance(actual, "builtins", "bool"),
            SupportedTy::Bytes { .. } => self.exact_instance(actual, "builtins", "bytes"),
            SupportedTy::Class { module, name, .. } => {
                relation_to_match(chalk_exact_class_object(self.db, actual, module, name))
            }
            SupportedTy::Counter { items, .. } => {
                self.match_unary_container(actual, ChalkContainerKind::Counter, items, depth)
            }
            SupportedTy::Date { .. } => self.exact_instance(actual, "datetime", "date"),
            SupportedTy::DateTime { .. } => self.exact_instance(actual, "datetime", "datetime"),
            SupportedTy::Dict {
                key_type,
                value_type,
                ..
            } => self.match_mapping_container(actual, key_type, value_type, depth),
            SupportedTy::Float { .. } => self
                .exact_instance(actual, "builtins", "float")
                .any(self.exact_instance(actual, "builtins", "int")),
            SupportedTy::FrozenSet { items, .. } => {
                self.match_unary_container(actual, ChalkContainerKind::FrozenSet, items, depth)
            }
            SupportedTy::Generator { items, .. } => {
                self.match_unary_container(actual, ChalkContainerKind::Generator, items, depth)
            }
            SupportedTy::HashlibHash { .. } => [
                ("_hashlib", "HASH"),
                ("_hashlib", "HASHXOF"),
                ("_blake2", "blake2b"),
                ("_blake2", "blake2s"),
            ]
            .into_iter()
            .map(|(module, name)| self.instance_derived_from(actual, module, name))
            .fold(TypeMatch::NoMatch, TypeMatch::any),
            SupportedTy::Int { .. } => self.exact_instance(actual, "builtins", "int"),
            SupportedTy::Iterable { items, .. } => {
                self.match_unary_container(actual, ChalkContainerKind::Iterable, items, depth)
            }
            SupportedTy::Json { .. } => self.match_json_container(actual, depth),
            SupportedTy::List { items, .. } => {
                self.match_unary_container(actual, ChalkContainerKind::List, items, depth)
            }
            SupportedTy::Module { name, .. } => {
                if chalk_module_is(self.db, actual, name) {
                    TypeMatch::Match
                } else {
                    TypeMatch::NoMatch
                }
            }
            SupportedTy::None { .. } => TypeMatch::NoMatch,
            SupportedTy::ReMatch { .. } => self.instance_derived_from(actual, "re", "Match"),
            SupportedTy::RePattern { .. } => self.instance_derived_from(actual, "re", "Pattern"),
            SupportedTy::RequestsHttpResponse { .. } => {
                self.instance_derived_from(actual, "requests.models", "Response")
            }
            SupportedTy::Set { items, .. } => {
                self.match_unary_container(actual, ChalkContainerKind::Set, items, depth)
            }
            SupportedTy::SequenceMatcher { .. } => {
                self.instance_derived_from(actual, "difflib", "SequenceMatcher")
            }
            SupportedTy::Str { .. } => self.exact_instance(actual, "builtins", "str"),
            SupportedTy::SubClassOf { ty_name, .. } => {
                self.match_accelerator_category(actual, ty_name, depth)
            }
            SupportedTy::Time { .. } => self.exact_instance(actual, "datetime", "time"),
            SupportedTy::Timedelta { .. } => self.exact_instance(actual, "datetime", "timedelta"),
            SupportedTy::TimeZone { .. } => self.exact_instance(actual, "datetime", "timezone"),
            SupportedTy::Tuple {
                items, is_variable, ..
            } => self.match_tuple(actual, items, *is_variable, depth),
            SupportedTy::Other { name, .. } if name == "TySlice" => {
                self.exact_instance(actual, "builtins", "slice")
            }
            SupportedTy::Other { .. } => TypeMatch::Inconclusive,
        }
    }

    fn match_unary_container(
        &mut self,
        actual: Type<'db>,
        kind: ChalkContainerKind,
        expected_item: &SupportedTy,
        depth: usize,
    ) -> TypeMatch {
        match chalk_container_type(self.db, actual, kind) {
            ChalkContainerType::Unary(actual_item) => {
                self.descend(actual_item, Expected::Registry(expected_item), depth)
            }
            ChalkContainerType::Unavailable => TypeMatch::Inconclusive,
            ChalkContainerType::NotContainer
            | ChalkContainerType::Mapping { .. }
            | ChalkContainerType::Tuple(_) => TypeMatch::NoMatch,
        }
    }

    fn match_mapping_container(
        &mut self,
        actual: Type<'db>,
        expected_key: &SupportedTy,
        expected_value: &SupportedTy,
        depth: usize,
    ) -> TypeMatch {
        match chalk_container_type(self.db, actual, ChalkContainerKind::Dict) {
            ChalkContainerType::Mapping { key, value } => {
                let key_result = self.descend(key, Expected::Registry(expected_key), depth);
                let value_result = self.descend(value, Expected::Registry(expected_value), depth);
                key_result.all(value_result)
            }
            ChalkContainerType::Unavailable => TypeMatch::Inconclusive,
            ChalkContainerType::NotContainer
            | ChalkContainerType::Unary(_)
            | ChalkContainerType::Tuple(_) => TypeMatch::NoMatch,
        }
    }

    fn match_tuple(
        &mut self,
        actual: Type<'db>,
        expected_items: &[SupportedTy],
        expected_variable: bool,
        depth: usize,
    ) -> TypeMatch {
        let actual = match chalk_container_type(self.db, actual, ChalkContainerKind::Tuple) {
            ChalkContainerType::Tuple(actual) => actual,
            ChalkContainerType::Unavailable => return TypeMatch::Inconclusive,
            _ => return TypeMatch::NoMatch,
        };
        let Some(depth) = self.next_depth(depth) else {
            return TypeMatch::Inconclusive;
        };

        if expected_variable {
            let Some(expected_item) = expected_items.first() else {
                return TypeMatch::NoMatch;
            };
            return TypeMatch::all_results(actual.elements.iter().map(|item| {
                self.match_type_at_depth(*item, Expected::Registry(expected_item), depth)
            }));
        }
        if actual.is_variable || actual.elements.len() != expected_items.len() {
            return TypeMatch::NoMatch;
        }
        TypeMatch::all_results(actual.elements.iter().zip(expected_items).map(
            |(actual, expected)| {
                self.match_type_at_depth(*actual, Expected::Registry(expected), depth)
            },
        ))
    }

    fn match_json_container(&mut self, actual: Type<'db>, depth: usize) -> TypeMatch {
        let list_result = match chalk_container_type(self.db, actual, ChalkContainerKind::List) {
            ChalkContainerType::Unary(item) => self.descend(item, Expected::JsonValue, depth),
            ChalkContainerType::Unavailable => TypeMatch::Inconclusive,
            _ => TypeMatch::NoMatch,
        };
        let dict_result = match chalk_container_type(self.db, actual, ChalkContainerKind::Dict) {
            ChalkContainerType::Mapping { key, value } => {
                let key_result = self.descend(key, Expected::JsonStringKey, depth);
                let value_result = self.descend(value, Expected::JsonValue, depth);
                key_result.all(value_result)
            }
            ChalkContainerType::Unavailable => TypeMatch::Inconclusive,
            _ if chalk_is_logical_struct(self.db, actual) => TypeMatch::Inconclusive,
            _ => TypeMatch::NoMatch,
        };
        list_result.any(dict_result)
    }

    fn match_json_value(&mut self, actual: Type<'db>, depth: usize) -> TypeMatch {
        if actual.is_none(self.db) {
            return TypeMatch::Match;
        }
        let primitive = [
            ("builtins", "bool"),
            ("builtins", "int"),
            ("builtins", "float"),
            ("builtins", "str"),
        ]
        .into_iter()
        .map(|(module, name)| self.exact_instance(actual, module, name))
        .fold(TypeMatch::NoMatch, TypeMatch::any);
        if primitive != TypeMatch::NoMatch {
            return primitive;
        }
        self.match_json_container(actual, depth)
    }

    fn match_accelerator_category(
        &mut self,
        actual: Type<'db>,
        name: &str,
        depth: usize,
    ) -> TypeMatch {
        match name {
            "TyEnum" => relation_to_match(chalk_is_enum(self.db, actual)),
            "TyJson" => self.match_json_container(actual, depth),
            "TyLogicalStruct" => {
                if chalk_is_logical_struct(self.db, actual) {
                    TypeMatch::Match
                } else {
                    TypeMatch::NoMatch
                }
            }
            "TyTuple" => match chalk_container_type(self.db, actual, ChalkContainerKind::Tuple) {
                ChalkContainerType::Tuple(_) => TypeMatch::Match,
                ChalkContainerType::Unavailable => TypeMatch::Inconclusive,
                _ => TypeMatch::NoMatch,
            },
            "TyProto" => self.derived_from(actual, "google.protobuf.message", "Message"),
            "TyProtoEnum" => self
                .derived_from(
                    actual,
                    "google.protobuf.internal.enum_type_wrapper",
                    "EnumTypeWrapper",
                )
                .any(self.derived_from(actual, "enum", "Enum")),
            _ => TypeMatch::Inconclusive,
        }
    }

    fn exact_instance(&self, actual: Type<'db>, module: &str, name: &str) -> TypeMatch {
        relation_to_match(chalk_exact_instance_class(self.db, actual, module, name))
    }

    fn derived_from(&self, actual: Type<'db>, module: &str, name: &str) -> TypeMatch {
        relation_to_match(chalk_class_derived_from(self.db, actual, module, name))
    }

    fn instance_derived_from(&self, actual: Type<'db>, module: &str, name: &str) -> TypeMatch {
        relation_to_match(chalk_instance_derived_from(self.db, actual, module, name))
    }

    fn descend(&mut self, actual: Type<'db>, expected: Expected<'_>, depth: usize) -> TypeMatch {
        let Some(depth) = self.next_depth(depth) else {
            return TypeMatch::Inconclusive;
        };
        self.match_type_at_depth(actual, expected, depth)
    }

    fn next_depth(&self, depth: usize) -> Option<usize> {
        (depth < self.expansion_limit).then(|| depth + 1)
    }
}

fn relation_to_match(relation: ChalkClassRelation) -> TypeMatch {
    match relation {
        ChalkClassRelation::Match => TypeMatch::Match,
        ChalkClassRelation::NoMatch => TypeMatch::NoMatch,
        ChalkClassRelation::Unavailable => TypeMatch::Inconclusive,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ruff_db::Db as _;
    use ruff_db::files::{File, system_path_to_file};
    use ruff_db::parsed::parsed_module;
    use ruff_db::system::{
        DbWithTestSystem as _, DbWithWritableSystem as _, SystemPath, SystemPathBuf,
    };
    use ruff_python_ast::PythonVersion;
    use ruff_python_ast::{
        self as ast,
        visitor::source_order::{self, SourceOrderVisitor},
    };
    use ty_module_resolver::SearchPathSettings;
    use ty_project::{ProjectMetadata, TestDb};
    use ty_python_core::platform::PythonPlatform;
    use ty_python_core::program::{FallibleStrategy, Program, ProgramSettings};
    use ty_python_semantic::chalk::{ChalkClassRelation, chalk_receiver_module_relation};
    use ty_python_semantic::types::Type;
    use ty_python_semantic::{
        HasType, PythonVersionSource, PythonVersionWithSource, SemanticModel,
    };

    use crate::supported_functions::{SupportedTy, current_supported_functions};

    use super::{Expected, Matcher, TypeMatch, match_supported_type};

    fn setup(main: &str, files: &[(&str, &str)]) -> (TestDb, File) {
        let project = ProjectMetadata::new("test", SystemPathBuf::from("/"));
        let mut db = TestDb::new(project);
        db.init_program().unwrap();

        for (path, source) in files {
            if let Some(parent) = SystemPath::new(path).parent() {
                db.memory_file_system()
                    .create_directory_all(parent)
                    .unwrap();
            }
            db.write_file(SystemPath::new(path), source).unwrap();
        }
        db.write_file(SystemPath::new("/main.py"), main).unwrap();
        let file = system_path_to_file(&db, "/main.py").unwrap();
        (db, file)
    }

    fn setup_with_site_packages(main: &str, files: &[(&str, &str)]) -> (TestDb, File) {
        let project = ProjectMetadata::new("test", SystemPathBuf::from("/src"));
        let mut db = TestDb::new(project);
        for path in ["/src", "/site-packages"] {
            db.memory_file_system()
                .create_directory_all(SystemPath::new(path))
                .unwrap();
        }
        for (path, source) in files {
            if let Some(parent) = SystemPath::new(path).parent() {
                db.memory_file_system()
                    .create_directory_all(parent)
                    .unwrap();
            }
            db.write_file(SystemPath::new(path), source).unwrap();
        }
        db.write_file(SystemPath::new("/src/main.py"), main)
            .unwrap();
        let search_paths = SearchPathSettings {
            extra_paths: Vec::new(),
            src_roots: vec![SystemPathBuf::from("/src")],
            custom_typeshed: None,
            site_packages_paths: vec![SystemPathBuf::from("/site-packages")],
            real_stdlib_path: None,
        }
        .to_search_paths(db.system(), db.vendored(), &FallibleStrategy)
        .unwrap();
        Program::from_settings(
            &db,
            ProgramSettings {
                python_version: PythonVersionWithSource {
                    version: PythonVersion::latest_ty(),
                    source: PythonVersionSource::Default,
                },
                python_platform: PythonPlatform::default(),
                search_paths,
            },
        );
        let file = system_path_to_file(&db, "/src/main.py").unwrap();
        (db, file)
    }

    fn argument_types(db: &TestDb, file: File) -> Vec<Type<'_>> {
        let parsed = parsed_module(db, file).load(db);
        let mut collector = SinkArgumentCollector {
            model: SemanticModel::new(db, file),
            types: Vec::new(),
        };
        collector.visit_body(&parsed.syntax().body);
        collector.types
    }

    struct SinkArgumentCollector<'db> {
        model: SemanticModel<'db>,
        types: Vec<Type<'db>>,
    }

    impl SourceOrderVisitor<'_> for SinkArgumentCollector<'_> {
        fn visit_expr(&mut self, expression: &ast::Expr) {
            if let ast::Expr::Call(call) = expression
                && call
                    .func
                    .as_name_expr()
                    .is_some_and(|name| name.id == "sink")
            {
                self.types.extend(
                    call.arguments
                        .args
                        .iter()
                        .map(|argument| argument.inferred_type(&self.model).unwrap()),
                );
            }
            source_order::walk_expr(self, expression);
        }
    }

    #[test]
    fn all_aggregation_prefers_known_mismatch() {
        assert_eq!(
            TypeMatch::Inconclusive.all(TypeMatch::NoMatch),
            TypeMatch::NoMatch
        );
    }

    #[test]
    fn any_aggregation_prefers_match() {
        assert_eq!(
            TypeMatch::Inconclusive.any(TypeMatch::Match),
            TypeMatch::Match
        );
    }

    #[test]
    fn primitives_literals_numeric_widening_and_baked_types() {
        let (db, file) = setup(
            "\
from datetime import date, datetime, time, timedelta, timezone
from difflib import SequenceMatcher
from re import Match, Pattern
from typing import cast

sink(
    True,
    b\"bytes\",
    1,
    1.5,
    \"text\",
    cast(date, object()),
    cast(datetime, object()),
    cast(time, object()),
    cast(timedelta, object()),
    cast(timezone, object()),
    cast(Match[str], object()),
    cast(Pattern[str], object()),
    SequenceMatcher(),
    slice(1),
)
",
            &[],
        );
        let types = argument_types(&db, file);
        let expected = [
            SupportedTy::Bool { nullable: false },
            SupportedTy::Bytes { nullable: false },
            SupportedTy::Float { nullable: false },
            SupportedTy::Float { nullable: false },
            SupportedTy::Str { nullable: false },
            SupportedTy::Date { nullable: false },
            SupportedTy::DateTime { nullable: false },
            SupportedTy::Time { nullable: false },
            SupportedTy::Timedelta { nullable: false },
            SupportedTy::TimeZone { nullable: false },
            SupportedTy::ReMatch { nullable: false },
            SupportedTy::RePattern { nullable: false },
            SupportedTy::SequenceMatcher { nullable: false },
            SupportedTy::Other {
                nullable: false,
                name: "TySlice".to_string(),
            },
        ];

        assert!(types.iter().zip(&expected).all(|(actual, expected)| {
            match_supported_type(&db, *actual, expected) == TypeMatch::Match
        }));
        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Int { nullable: false }),
            TypeMatch::NoMatch
        );
    }

    #[test]
    fn nullable_and_dynamic_types_are_binding() {
        let (db, file) = setup(
            "\
from typing import Any, cast

any_value = cast(Any, 1)
sink(None, 1, any_value)
",
            &[],
        );
        let types = argument_types(&db, file);

        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Int { nullable: true }),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Int { nullable: false }),
            TypeMatch::NoMatch
        );
        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::None { nullable: false }),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[1], &SupportedTy::None { nullable: true }),
            TypeMatch::NoMatch
        );
        assert_eq!(
            match_supported_type(&db, types[2], &SupportedTy::Any { nullable: true }),
            TypeMatch::Inconclusive
        );
        assert_eq!(
            match_supported_type(&db, Type::unknown(), &SupportedTy::Any { nullable: true }),
            TypeMatch::Inconclusive
        );
    }

    #[test]
    fn unions_require_every_arm_and_preserve_known_mismatches() {
        let (db, file) = setup(
            "\
from typing import Any, cast

all_supported = cast(int | float, 1)
one_mismatch = cast(int | str, 1)
unknown = cast(int | Any, 1)
mismatch_and_unknown = cast(str | Any, \"\")
sink(all_supported, one_mismatch, unknown, mismatch_and_unknown)
",
            &[],
        );
        let types = argument_types(&db, file);

        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Float { nullable: false }),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[1], &SupportedTy::Int { nullable: false }),
            TypeMatch::NoMatch
        );
        assert_eq!(
            match_supported_type(&db, types[2], &SupportedTy::Int { nullable: false }),
            TypeMatch::Inconclusive
        );
        assert_eq!(
            match_supported_type(&db, types[3], &SupportedTy::Int { nullable: false }),
            TypeMatch::NoMatch
        );
    }

    #[test]
    fn type_var_constraints_expand_but_upper_bounds_do_not() {
        let (db, file) = setup(
            "\
def bounded[T: int](value: T):
    sink(value)

def constrained[T: (int, str)](value: T):
    sink(value)
",
            &[],
        );
        let types = argument_types(&db, file);

        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Int { nullable: false }),
            TypeMatch::Inconclusive
        );
        assert_eq!(
            match_supported_type(&db, types[1], &SupportedTy::Int { nullable: false }),
            TypeMatch::NoMatch
        );
        assert_eq!(
            match_supported_type(&db, types[1], &SupportedTy::Any { nullable: false }),
            TypeMatch::Match
        );
    }

    #[test]
    fn recursive_containers_and_partial_unknowns_terminate() {
        let (db, file) = setup(
            "\
from collections import Counter
from typing import Any, cast

type Recursive = list[Recursive]
recursive = cast(Recursive, [])
partial = cast(list[str | Any], [])
mapping = cast(dict[str, list[int]], {})
counter = cast(Counter[str], {})
generator = (item for item in [1])
sink(recursive, partial, mapping, counter, generator, [1], {1}, frozenset({1}))
",
            &[],
        );
        let types = argument_types(&db, file);

        assert_eq!(
            match_supported_type(
                &db,
                types[0],
                &SupportedTy::List {
                    nullable: false,
                    items: Box::new(SupportedTy::List {
                        nullable: false,
                        items: Box::new(SupportedTy::Any { nullable: true }),
                    }),
                }
            ),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Json { nullable: false }),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[1],
                &SupportedTy::List {
                    nullable: false,
                    items: Box::new(SupportedTy::Int { nullable: false }),
                }
            ),
            TypeMatch::NoMatch
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[2],
                &SupportedTy::Dict {
                    nullable: false,
                    key_type: Box::new(SupportedTy::Str { nullable: false }),
                    value_type: Box::new(SupportedTy::List {
                        nullable: false,
                        items: Box::new(SupportedTy::Int { nullable: false }),
                    }),
                }
            ),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[3],
                &SupportedTy::Counter {
                    nullable: false,
                    items: Box::new(SupportedTy::Str { nullable: false }),
                }
            ),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[4],
                &SupportedTy::Generator {
                    nullable: false,
                    items: Box::new(SupportedTy::Int { nullable: false }),
                }
            ),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[5],
                &SupportedTy::Iterable {
                    nullable: false,
                    items: Box::new(SupportedTy::Int { nullable: false }),
                }
            ),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[6],
                &SupportedTy::Set {
                    nullable: false,
                    items: Box::new(SupportedTy::Int { nullable: false }),
                }
            ),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[7],
                &SupportedTy::FrozenSet {
                    nullable: false,
                    items: Box::new(SupportedTy::Int { nullable: false }),
                }
            ),
            TypeMatch::Match
        );
    }

    #[test]
    fn fixed_and_variable_tuples_match_native_specs() {
        let (db, file) = setup(
            "\
from typing import cast

fixed = cast(tuple[int, str], (1, \"\"))
variable = cast(tuple[int, ...], ())
sink(fixed, variable)
",
            &[],
        );
        let types = argument_types(&db, file);
        let fixed = SupportedTy::Tuple {
            nullable: false,
            items: vec![
                SupportedTy::Int { nullable: false },
                SupportedTy::Str { nullable: false },
            ],
            is_variable: false,
        };
        let variable = SupportedTy::Tuple {
            nullable: false,
            items: vec![SupportedTy::Int { nullable: false }],
            is_variable: true,
        };

        assert_eq!(
            match_supported_type(&db, types[0], &fixed),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[0], &variable),
            TypeMatch::NoMatch
        );
        assert_eq!(
            match_supported_type(&db, types[1], &variable),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[1], &fixed),
            TypeMatch::NoMatch
        );
    }

    #[test]
    fn class_and_module_matching_use_semantic_identity() {
        const PKG: &str = "class Token: ...\nclass Outer:\n    class Token: ...\n";
        const OTHER: &str = "class Token: ...\n";
        let (db, file) = setup_with_site_packages(
            "\
import pkg
from other import Token as OtherToken
from pkg import Outer, Token

sink(pkg, Token, OtherToken, Outer.Token)
",
            &[
                ("/site-packages/pkg/__init__.pyi", PKG),
                ("/site-packages/pkg/__init__.py", PKG),
                ("/site-packages/other.pyi", OTHER),
                ("/site-packages/other.py", OTHER),
            ],
        );
        let types = argument_types(&db, file);
        let expected_class = SupportedTy::Class {
            nullable: false,
            module: "pkg".to_string(),
            name: "Token".to_string(),
        };

        assert_eq!(
            match_supported_type(
                &db,
                types[0],
                &SupportedTy::Module {
                    nullable: false,
                    name: "pkg".to_string(),
                }
            ),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[1], &expected_class),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[2], &expected_class),
            TypeMatch::NoMatch
        );
        assert_eq!(
            match_supported_type(&db, types[3], &expected_class),
            TypeMatch::NoMatch
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[1],
                &SupportedTy::Module {
                    nullable: false,
                    name: "pkg".to_string(),
                }
            ),
            TypeMatch::NoMatch
        );
    }

    #[test]
    fn local_runtime_shadow_does_not_match_vendored_registry_identity() {
        let (db, file) = setup(
            "\
import datetime

sink(datetime, datetime.datetime)
",
            &[(
                "/datetime.py",
                "class datetime:\n    @classmethod\n    def now(cls): ...\n",
            )],
        );
        let types = argument_types(&db, file);

        assert_eq!(
            chalk_receiver_module_relation(&db, types[0], "datetime"),
            ChalkClassRelation::NoMatch
        );
        assert_eq!(
            chalk_receiver_module_relation(&db, types[1], "datetime.datetime"),
            ChalkClassRelation::NoMatch
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[1],
                &SupportedTy::Class {
                    nullable: false,
                    module: "datetime".to_string(),
                    name: "datetime".to_string(),
                }
            ),
            TypeMatch::NoMatch
        );
    }

    #[test]
    fn baked_ancestry_types_match_instances_only() {
        let (db, file) = setup_with_site_packages(
            "\
from _hashlib import HASH
from difflib import SequenceMatcher
from re import Match, Pattern
from requests.models import Response
from typing import cast

sink(
    cast(Match[str], object()),
    Match,
    cast(Pattern[str], object()),
    Pattern,
    SequenceMatcher(),
    SequenceMatcher,
    cast(Response, object()),
    Response,
    cast(HASH, object()),
    HASH,
)
",
            &[
                ("/site-packages/requests/__init__.pyi", ""),
                ("/site-packages/requests/__init__.py", ""),
                (
                    "/site-packages/requests/models.pyi",
                    "class Response: ...\n",
                ),
                ("/site-packages/requests/models.py", "class Response: ...\n"),
                ("/site-packages/_hashlib.pyi", "class HASH: ...\n"),
                ("/site-packages/_hashlib.py", "class HASH: ...\n"),
            ],
        );
        let types = argument_types(&db, file);
        let expected = [
            SupportedTy::ReMatch { nullable: false },
            SupportedTy::ReMatch { nullable: false },
            SupportedTy::RePattern { nullable: false },
            SupportedTy::RePattern { nullable: false },
            SupportedTy::SequenceMatcher { nullable: false },
            SupportedTy::SequenceMatcher { nullable: false },
            SupportedTy::RequestsHttpResponse { nullable: false },
            SupportedTy::RequestsHttpResponse { nullable: false },
            SupportedTy::HashlibHash { nullable: false },
            SupportedTy::HashlibHash { nullable: false },
        ];

        for (index, (actual, expected)) in types.iter().zip(&expected).enumerate() {
            assert_eq!(
                match_supported_type(&db, *actual, expected),
                if index % 2 == 0 {
                    TypeMatch::Match
                } else {
                    TypeMatch::NoMatch
                },
                "argument {index}"
            );
        }
    }

    #[test]
    fn accelerator_categories_use_native_shape_and_ancestry() {
        let (db, file) = setup_with_site_packages(
            "\
from enum import Enum
from google.protobuf.internal.enum_type_wrapper import EnumTypeWrapper
from google.protobuf.message import Message
from typing import TypedDict, cast

class Color(Enum):
    RED = 1

class Row(TypedDict):
    value: int

class Event(Message): ...
class ProtoEnum(EnumTypeWrapper): ...

row = cast(Row, {})
json_value = cast(dict[str, list[int | None]], {})
bad_json = cast(dict[int, str], {})
sink(Color.RED, Color, row, json_value, bad_json, Event(), Event, ProtoEnum())
",
            &[
                ("/site-packages/google/__init__.pyi", ""),
                ("/site-packages/google/__init__.py", ""),
                ("/site-packages/google/protobuf/__init__.pyi", ""),
                ("/site-packages/google/protobuf/__init__.py", ""),
                (
                    "/site-packages/google/protobuf/message.pyi",
                    "class Message: ...\n",
                ),
                (
                    "/site-packages/google/protobuf/message.py",
                    "class Message: ...\n",
                ),
                ("/site-packages/google/protobuf/internal/__init__.pyi", ""),
                ("/site-packages/google/protobuf/internal/__init__.py", ""),
                (
                    "/site-packages/google/protobuf/internal/enum_type_wrapper.pyi",
                    "class EnumTypeWrapper: ...\n",
                ),
                (
                    "/site-packages/google/protobuf/internal/enum_type_wrapper.py",
                    "class EnumTypeWrapper: ...\n",
                ),
            ],
        );
        let types = argument_types(&db, file);

        for (actual, category, expected) in [
            (types[0], "TyEnum", TypeMatch::Match),
            (types[1], "TyEnum", TypeMatch::Match),
            (types[2], "TyLogicalStruct", TypeMatch::Match),
            (types[3], "TyJson", TypeMatch::Match),
            (types[4], "TyJson", TypeMatch::NoMatch),
            (types[5], "TyProto", TypeMatch::Match),
            (types[6], "TyProto", TypeMatch::Match),
            (types[7], "TyProtoEnum", TypeMatch::Match),
        ] {
            assert_eq!(
                match_supported_type(
                    &db,
                    actual,
                    &SupportedTy::SubClassOf {
                        ty_name: category.to_string(),
                        match_nullable: false,
                    }
                ),
                expected,
                "{category}"
            );
        }
        assert_eq!(
            match_supported_type(&db, types[3], &SupportedTy::Json { nullable: false }),
            TypeMatch::Match
        );
    }

    #[test]
    fn json_dict_keys_are_matched_recursively() {
        let (db, file) = setup(
            "\
from typing import Any, cast

string_or_any = cast(dict[str | Any, int], {})
int_or_any = cast(dict[int | Any, int], {})
sink(string_or_any, int_or_any)
",
            &[],
        );
        let types = argument_types(&db, file);
        let json = SupportedTy::Json { nullable: false };

        assert_eq!(
            match_supported_type(&db, types[0], &json),
            TypeMatch::Inconclusive
        );
        assert_eq!(
            match_supported_type(&db, types[1], &json),
            TypeMatch::NoMatch
        );
    }

    #[test]
    fn json_container_alternatives_prefer_a_constructible_match() {
        let (db, file) = setup(
            "\
from typing import Any, cast

DynamicBase: Any

class JsonDict(DynamicBase, dict[str, int]): ...

sink(cast(JsonDict, object()))
",
            &[],
        );
        let types = argument_types(&db, file);

        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Json { nullable: false }),
            TypeMatch::Match
        );
    }

    #[test]
    fn intersections_require_positive_proof_or_consistent_bounds() {
        let (db, file) = setup(
            "\
def narrowed[T](value: T):
    if isinstance(value, str):
        sink(value)
",
            &[],
        );
        let types = argument_types(&db, file);

        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Str { nullable: false }),
            TypeMatch::Match
        );
        assert_eq!(
            match_supported_type(&db, types[0], &SupportedTy::Int { nullable: false }),
            TypeMatch::Inconclusive
        );
    }

    #[test]
    fn current_snapshot_uses_only_intentionally_supported_category_names() {
        fn collect<'a>(
            ty: &'a SupportedTy,
            subclass_names: &mut BTreeSet<&'a str>,
            other_names: &mut BTreeSet<&'a str>,
        ) {
            match ty {
                SupportedTy::SubClassOf { ty_name, .. } => {
                    subclass_names.insert(ty_name);
                }
                SupportedTy::Other { name, .. } => {
                    other_names.insert(name);
                }
                SupportedTy::Counter { items, .. }
                | SupportedTy::FrozenSet { items, .. }
                | SupportedTy::Generator { items, .. }
                | SupportedTy::Iterable { items, .. }
                | SupportedTy::List { items, .. }
                | SupportedTy::Set { items, .. } => collect(items, subclass_names, other_names),
                SupportedTy::Dict {
                    key_type,
                    value_type,
                    ..
                } => {
                    collect(key_type, subclass_names, other_names);
                    collect(value_type, subclass_names, other_names);
                }
                SupportedTy::Tuple { items, .. } => {
                    for item in items {
                        collect(item, subclass_names, other_names);
                    }
                }
                _ => {}
            }
        }

        let mut subclass_names = BTreeSet::new();
        let mut other_names = BTreeSet::new();
        for (_, signatures) in current_supported_functions().entries() {
            for signature in signatures {
                for argument in signature.args() {
                    collect(argument.ty(), &mut subclass_names, &mut other_names);
                }
            }
        }

        assert_eq!(
            subclass_names,
            BTreeSet::from([
                "TyEnum",
                "TyJson",
                "TyLogicalStruct",
                "TyProto",
                "TyProtoEnum",
                "TyTuple",
            ])
        );
        assert_eq!(other_names, BTreeSet::from(["TySlice"]));
    }

    #[test]
    fn unknown_matcher_categories_are_inconclusive() {
        let (db, file) = setup("sink(1)\n", &[]);
        let types = argument_types(&db, file);

        assert_eq!(
            match_supported_type(
                &db,
                types[0],
                &SupportedTy::SubClassOf {
                    ty_name: "FutureCategory".to_string(),
                    match_nullable: false,
                }
            ),
            TypeMatch::Inconclusive
        );
        assert_eq!(
            match_supported_type(
                &db,
                types[0],
                &SupportedTy::Other {
                    name: "FutureOther".to_string(),
                    nullable: false,
                }
            ),
            TypeMatch::Inconclusive
        );
    }

    #[test]
    fn expansion_overflow_is_inconclusive_but_does_not_hide_a_mismatch() {
        let (db, file) = setup(
            "\
from typing import cast

deep = cast(list[list[int]], [])
deep_or_mismatch = cast(list[list[int]] | str, [])
sink(deep, deep_or_mismatch)
",
            &[],
        );
        let types = argument_types(&db, file);
        let expected = SupportedTy::List {
            nullable: false,
            items: Box::new(SupportedTy::List {
                nullable: false,
                items: Box::new(SupportedTy::Int { nullable: false }),
            }),
        };

        assert_eq!(
            Matcher::new(&db, 0).match_type(types[0], Expected::Registry(&expected)),
            TypeMatch::Inconclusive
        );
        assert_eq!(
            Matcher::new(&db, 1).match_type(types[1], Expected::Registry(&expected)),
            TypeMatch::NoMatch
        );
    }

    #[test]
    fn expansion_limit_is_independent_of_union_arm_order() {
        let (db, file) = setup(
            "\
from typing import cast

deep_first = cast(list[list[list[int]]] | list[str], [])
mismatch_first = cast(list[str] | list[list[list[int]]], [])
sink(deep_first, mismatch_first)
",
            &[],
        );
        let types = argument_types(&db, file);
        let expected = SupportedTy::List {
            nullable: false,
            items: Box::new(SupportedTy::List {
                nullable: false,
                items: Box::new(SupportedTy::List {
                    nullable: false,
                    items: Box::new(SupportedTy::Int { nullable: false }),
                }),
            }),
        };

        for actual in types {
            assert_eq!(
                Matcher::new(&db, 2).match_type(actual, Expected::Registry(&expected)),
                TypeMatch::NoMatch
            );
        }
    }
}
