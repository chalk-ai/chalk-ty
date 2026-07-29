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
