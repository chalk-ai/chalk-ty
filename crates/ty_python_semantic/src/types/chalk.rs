use ruff_db::files::File;
use ty_module_resolver::{Module, ModuleName, file_to_module, resolve_module};

use super::class::{CodeGeneratorKind, FieldKind};
use super::constraints::ConstraintSetBuilder;
use super::generics::InferableTypeVars;
use super::instance::SynthesizedProtocolKind;
use super::{
    ClassBase, ClassType, DataclassFlags, KnownClass, SubclassOfInner, Type,
    TypeVarBoundOrConstraints,
};
use crate::chalk::{
    ChalkClassRelation, ChalkContainerKind, ChalkContainerType, ChalkIntersection, ChalkTupleType,
    ChalkTypeShape, ModuleOrigin, module_ownership_origin,
};
use crate::db::Db;
use crate::place::{DefinedPlace, Definedness, Place, PlaceAndQualifiers, imported_symbol};

const CHALK_FEATURES_MODULE: &str = "chalk.features";
const CHALK_EXTENSIONS_MODULE: &str = "ty_chalk_extensions";

impl<'db> Type<'db> {
    pub(crate) fn chalk_type_shape(self, db: &'db dyn Db) -> ChalkTypeShape<'db> {
        match self {
            Type::Dynamic(_) => ChalkTypeShape::Dynamic,
            Type::Divergent(_) => self
                .materialized_divergent_fallback()
                .map_or(ChalkTypeShape::Unavailable, ChalkTypeShape::Expanded),
            Type::Never => ChalkTypeShape::Never,
            Type::TypeAlias(alias) => ChalkTypeShape::Expanded(alias.value_type(db)),
            Type::NewTypeInstance(newtype) => {
                ChalkTypeShape::Expanded(newtype.concrete_base_type(db))
            }
            Type::TypeVar(typevar) => {
                let Some(bound) = typevar.typevar(db).bound_or_constraints(db) else {
                    return ChalkTypeShape::Unavailable;
                };
                match bound {
                    // An upper bound does not mean that the type variable is exactly the bound.
                    TypeVarBoundOrConstraints::UpperBound(_) => ChalkTypeShape::Unavailable,
                    TypeVarBoundOrConstraints::Constraints(constraints) => {
                        ChalkTypeShape::Expanded(constraints.as_type(db))
                    }
                }
            }
            Type::EnumComplement(complement) => {
                ChalkTypeShape::Expanded(complement.remaining_literal_union(db))
            }
            Type::Union(union) => ChalkTypeShape::Union(union.elements(db)),
            Type::Intersection(intersection) => ChalkTypeShape::Intersection(ChalkIntersection {
                positive: intersection.positive(db).iter().copied().collect(),
                top_materialization: self.top_materialization(db),
                bottom_materialization: self.bottom_materialization(db),
            }),
            Type::AlwaysTruthy | Type::AlwaysFalsy => ChalkTypeShape::Unavailable,
            _ => ChalkTypeShape::Concrete,
        }
    }

    pub(crate) fn chalk_container_type(
        self,
        db: &'db dyn Db,
        kind: ChalkContainerKind,
    ) -> ChalkContainerType<'db> {
        if matches!(self, Type::Dynamic(_) | Type::Divergent(_)) {
            return ChalkContainerType::Unavailable;
        }

        if kind == ChalkContainerKind::Tuple {
            if let Some(spec) = self.tuple_instance_spec(db) {
                return ChalkContainerType::Tuple(ChalkTupleType {
                    elements: spec.all_elements().into(),
                    is_variable: spec.is_variadic(),
                });
            }
            return if self.chalk_class_has_unavailable_base(db) {
                ChalkContainerType::Unavailable
            } else {
                ChalkContainerType::NotContainer
            };
        }

        if kind == ChalkContainerKind::Iterable {
            return match self.try_iterate(db) {
                Ok(spec) => ChalkContainerType::Unary(spec.homogeneous_element_type(db)),
                Err(_) if self.chalk_class_has_unavailable_base(db) => {
                    ChalkContainerType::Unavailable
                }
                Err(_) => ChalkContainerType::NotContainer,
            };
        }

        let known_classes: &[KnownClass] = match kind {
            ChalkContainerKind::List => &[KnownClass::List],
            ChalkContainerKind::Set => &[KnownClass::Set],
            ChalkContainerKind::FrozenSet => &[KnownClass::FrozenSet],
            ChalkContainerKind::Dict => &[KnownClass::Dict],
            ChalkContainerKind::Generator => &[KnownClass::GeneratorType, KnownClass::Generator],
            ChalkContainerKind::Counter => &[KnownClass::Counter],
            ChalkContainerKind::Tuple | ChalkContainerKind::Iterable => {
                return ChalkContainerType::NotContainer;
            }
        };

        let Some(types) = self.chalk_known_base_types(db, known_classes) else {
            return if self.chalk_class_has_unavailable_base(db) {
                ChalkContainerType::Unavailable
            } else {
                ChalkContainerType::NotContainer
            };
        };

        match kind {
            ChalkContainerKind::Dict => {
                let [key, value, ..] = types.as_ref() else {
                    return ChalkContainerType::Mapping {
                        key: Type::unknown(),
                        value: Type::unknown(),
                    };
                };
                ChalkContainerType::Mapping {
                    key: *key,
                    value: *value,
                }
            }
            ChalkContainerKind::List
            | ChalkContainerKind::Set
            | ChalkContainerKind::FrozenSet
            | ChalkContainerKind::Generator
            | ChalkContainerKind::Counter => {
                ChalkContainerType::Unary(types.first().copied().unwrap_or_else(Type::unknown))
            }
            ChalkContainerKind::Tuple | ChalkContainerKind::Iterable => {
                ChalkContainerType::NotContainer
            }
        }
    }

    pub(crate) fn chalk_exact_instance_class(
        self,
        db: &'db dyn Db,
        module: &str,
        name: &str,
    ) -> ChalkClassRelation {
        let Some(class) = self.nominal_class(db) else {
            return ChalkClassRelation::NoMatch;
        };
        class_identity_relation(db, class, module, name)
    }

    pub(crate) fn chalk_exact_class_object(
        self,
        db: &'db dyn Db,
        module: &str,
        name: &str,
    ) -> ChalkClassRelation {
        let class = match self {
            Type::ClassLiteral(class) => class.default_specialization(db),
            Type::GenericAlias(alias) => ClassType::Generic(alias),
            _ => return ChalkClassRelation::NoMatch,
        };
        class_identity_relation(db, class, module, name)
    }

    pub(crate) fn chalk_receiver_module(self, db: &'db dyn Db) -> Option<Module<'db>> {
        let (class, _) = self
            .chalk_class_type(db)
            .ok()
            .flatten()?
            .static_class_literal(db)?;
        file_to_module(db, class.file(db))
    }

    pub(crate) fn chalk_receiver_module_relation(
        self,
        db: &'db dyn Db,
        expected: &str,
    ) -> ChalkClassRelation {
        if matches!(self, Type::Dynamic(_) | Type::Divergent(_)) {
            return ChalkClassRelation::Unavailable;
        }
        if let Type::ModuleLiteral(module) = self {
            return registry_module_relation(db, module.module(db), expected);
        }

        let class = match self.chalk_class_type(db) {
            Ok(Some(class)) => class,
            Ok(None) => return ChalkClassRelation::NoMatch,
            Err(()) => return ChalkClassRelation::Unavailable,
        };
        registry_class_derived_from(db, class, expected)
    }

    pub(crate) fn chalk_class_derived_from(
        self,
        db: &'db dyn Db,
        module: &str,
        name: &str,
    ) -> ChalkClassRelation {
        let class = match self.chalk_class_type(db) {
            Ok(Some(class)) => class,
            Ok(None) => return ChalkClassRelation::NoMatch,
            Err(()) => return ChalkClassRelation::Unavailable,
        };
        class_derived_from(db, class, module, name)
    }

    pub(crate) fn chalk_instance_derived_from(
        self,
        db: &'db dyn Db,
        module: &str,
        name: &str,
    ) -> ChalkClassRelation {
        let Some(class) = self.nominal_class(db) else {
            return ChalkClassRelation::NoMatch;
        };
        class_derived_from(db, class, module, name)
    }

    pub(crate) fn chalk_module_is(self, db: &'db dyn Db, expected: &str) -> bool {
        let Type::ModuleLiteral(module) = self else {
            return false;
        };
        module.module(db).name(db).as_str() == expected
    }

    pub(crate) fn chalk_is_enum(self, db: &'db dyn Db) -> ChalkClassRelation {
        if self.is_enum(db) {
            return ChalkClassRelation::Match;
        }
        self.chalk_class_derived_from(db, "enum", "Enum")
    }

    pub(crate) fn chalk_is_logical_struct(self, _db: &'db dyn Db) -> bool {
        matches!(self, Type::TypedDict(_))
            || matches!(
                self,
                Type::ProtocolInstance(protocol)
                    if protocol.synthesized_kind() == Some(SynthesizedProtocolKind::ChalkFeatures)
            )
    }

    fn chalk_class_type(self, db: &'db dyn Db) -> Result<Option<ClassType<'db>>, ()> {
        match self {
            Type::ClassLiteral(class) => Ok(Some(class.default_specialization(db))),
            Type::GenericAlias(alias) => Ok(Some(ClassType::Generic(alias))),
            Type::SubclassOf(subclass) => match subclass.subclass_of() {
                SubclassOfInner::Class(class) => Ok(Some(class)),
                SubclassOfInner::Dynamic(_) | SubclassOfInner::TypeVar(_) => Err(()),
            },
            Type::TypedDict(typed_dict) => Ok(typed_dict.defining_class()),
            _ => Ok(self.nominal_class(db)),
        }
    }

    fn chalk_known_base_types(
        self,
        db: &'db dyn Db,
        known_classes: &[KnownClass],
    ) -> Option<Box<[Type<'db>]>> {
        let class = self.nominal_class(db)?;
        for base in class.iter_mro(db) {
            let ClassBase::Class(base) = base else {
                continue;
            };
            if !base
                .known(db)
                .is_some_and(|known| known_classes.contains(&known))
            {
                continue;
            }
            return Some(
                base.into_generic_alias()
                    .map(|alias| alias.specialization(db).types(db).into())
                    .unwrap_or_default(),
            );
        }
        None
    }

    fn chalk_class_has_unavailable_base(self, db: &'db dyn Db) -> bool {
        self.nominal_class(db).is_some_and(|class| {
            class.iter_mro(db).any(|base| {
                matches!(
                    base,
                    ClassBase::Any | ClassBase::Dynamic(_) | ClassBase::Divergent(_)
                )
            })
        })
    }
}

fn class_identity_relation(
    db: &dyn Db,
    class: ClassType<'_>,
    expected_module: &str,
    expected_name: &str,
) -> ChalkClassRelation {
    if class.name(db) != expected_name {
        return ChalkClassRelation::NoMatch;
    }
    let definition_file = class.class_literal(db).file(db);
    let Some(module) = file_to_module(db, definition_file) else {
        return ChalkClassRelation::Unavailable;
    };
    let origin_relation = registry_origin_relation(module_ownership_origin(
        db,
        definition_file,
        module.name(db),
        Some(module),
    ));
    if origin_relation != ChalkClassRelation::Match {
        return origin_relation;
    }
    if class
        .qualified_name(db)
        .components_excluding_self()
        .iter()
        .flat_map(|component| component.split('.'))
        .eq(expected_module.split('.'))
    {
        ChalkClassRelation::Match
    } else {
        ChalkClassRelation::NoMatch
    }
}

fn registry_module_relation(db: &dyn Db, module: Module<'_>, expected: &str) -> ChalkClassRelation {
    if module.name(db).as_str() != expected {
        return ChalkClassRelation::NoMatch;
    }
    let Some(typing_file) = module.file(db) else {
        return ChalkClassRelation::Unavailable;
    };
    registry_origin_relation(module_ownership_origin(
        db,
        typing_file,
        module.name(db),
        Some(module),
    ))
}

fn registry_class_derived_from(
    db: &dyn Db,
    class: ClassType<'_>,
    expected: &str,
) -> ChalkClassRelation {
    let mut unavailable = false;
    for base in class.iter_mro(db) {
        match base {
            ClassBase::Class(base) => match registry_class_relation(db, base, expected) {
                ChalkClassRelation::Match => return ChalkClassRelation::Match,
                ChalkClassRelation::Unavailable => unavailable = true,
                ChalkClassRelation::NoMatch => {}
            },
            ClassBase::Any | ClassBase::Dynamic(_) | ClassBase::Divergent(_) => unavailable = true,
            ClassBase::Protocol | ClassBase::Generic | ClassBase::TypedDict(_) => {}
        }
    }
    if unavailable {
        ChalkClassRelation::Unavailable
    } else {
        ChalkClassRelation::NoMatch
    }
}

fn registry_class_relation(
    db: &dyn Db,
    class: ClassType<'_>,
    expected: &str,
) -> ChalkClassRelation {
    let definition_file = class.class_literal(db).file(db);
    let Some(module) = file_to_module(db, definition_file) else {
        return ChalkClassRelation::Unavailable;
    };
    let origin_relation = registry_origin_relation(module_ownership_origin(
        db,
        definition_file,
        module.name(db),
        Some(module),
    ));
    if origin_relation != ChalkClassRelation::Match {
        return origin_relation;
    }

    let qualified_name = class.qualified_name(db);
    let matches = qualified_name
        .components_excluding_self()
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(class.name(db).as_str()))
        .eq(expected.split('.'));
    if matches {
        ChalkClassRelation::Match
    } else {
        ChalkClassRelation::NoMatch
    }
}

fn registry_origin_relation(origin: ModuleOrigin) -> ChalkClassRelation {
    match origin {
        ModuleOrigin::StandardLibrary | ModuleOrigin::ThirdParty => ChalkClassRelation::Match,
        ModuleOrigin::FirstParty | ModuleOrigin::Extra | ModuleOrigin::Other => {
            ChalkClassRelation::NoMatch
        }
        ModuleOrigin::Namespace | ModuleOrigin::Unresolved => ChalkClassRelation::Unavailable,
    }
}

fn class_derived_from(
    db: &dyn Db,
    class: ClassType<'_>,
    module: &str,
    name: &str,
) -> ChalkClassRelation {
    let mut unavailable = false;
    for base in class.iter_mro(db) {
        match base {
            ClassBase::Class(base) => match class_identity_relation(db, base, module, name) {
                ChalkClassRelation::Match => return ChalkClassRelation::Match,
                ChalkClassRelation::Unavailable => unavailable = true,
                ChalkClassRelation::NoMatch => {}
            },
            ClassBase::Any | ClassBase::Dynamic(_) | ClassBase::Divergent(_) => unavailable = true,
            ClassBase::Protocol | ClassBase::Generic | ClassBase::TypedDict(_) => {}
        }
    }

    if unavailable {
        ChalkClassRelation::Unavailable
    } else {
        ChalkClassRelation::NoMatch
    }
}

impl<'db> Type<'db> {
    pub(super) fn chalk_resolved_union_inference_arm(
        self,
        db: &'db dyn Db,
        actual: Type<'db>,
        constraints: &ConstraintSetBuilder<'db>,
        inferable: InferableTypeVars<'db>,
    ) -> Option<Type<'db>> {
        fn resolved_inner<'db>(db: &'db dyn Db, ty: Type<'db>) -> Option<Type<'db>> {
            let instance = ty.as_nominal_instance()?;
            if instance.class_name(db) != "Resolved"
                || instance.class_module_name(db)?.as_str() != CHALK_EXTENSIONS_MODULE
            {
                return None;
            }
            let (_, specialization) = ty.nominal_class(db)?.static_class_literal(db)?;
            specialization?.types(db).first().copied()
        }

        let Type::Union(union) = self else {
            return None;
        };
        let [left, right] = union.elements(db) else {
            return None;
        };

        let resolved_arm = if let (Some(typevar), Some(resolved_typevar)) = (
            left.as_typevar(),
            resolved_inner(db, *right).and_then(Type::as_typevar),
        ) && typevar == resolved_typevar
        {
            *right
        } else if let (Some(typevar), Some(resolved_typevar)) = (
            right.as_typevar(),
            resolved_inner(db, *left).and_then(Type::as_typevar),
        ) && typevar == resolved_typevar
        {
            *left
        } else {
            return None;
        };

        (!actual
            .when_assignable_to(db, resolved_arm, constraints, inferable)
            .is_never_satisfied(db))
        .then_some(resolved_arm)
    }

    pub(super) fn chalk_feature_value_type(self, db: &'db dyn Db, file: File) -> Type<'db> {
        if self
            .nominal_class(db)
            .is_none_or(|class| class.name(db) != "Primary")
        {
            return self;
        }
        let Some(module_name) = ModuleName::new_static(CHALK_FEATURES_MODULE) else {
            return self;
        };
        let Some(module) = resolve_module(db, file, &module_name) else {
            return self;
        };
        let Some(Type::ClassLiteral(primary)) =
            imported_symbol(db, module.file(db), "Primary", None).ignore_possibly_undefined()
        else {
            return self;
        };
        let Some(specialization) = primary
            .as_static()
            .and_then(|primary| self.specialization_of(db, primary))
        else {
            return self;
        };
        specialization.types(db).first().copied().unwrap_or(self)
    }

    pub(super) fn chalk_missing_return_fields(
        self,
        db: &'db dyn Db,
        actual: Type<'db>,
    ) -> Option<Vec<String>> {
        fn chalk_protocol<'db>(ty: Type<'db>) -> Option<super::ProtocolInstanceType<'db>> {
            let Type::ProtocolInstance(protocol) = ty else {
                return None;
            };
            (protocol.synthesized_kind() == Some(SynthesizedProtocolKind::ChalkFeatures))
                .then_some(protocol)
        }

        fn lookup_member<'db>(
            db: &'db dyn Db,
            ty: Type<'db>,
            name: &str,
        ) -> Option<DefinedPlace<'db>> {
            match ty.member(db, name).place {
                Place::Defined(member) => Some(member),
                Place::Undefined => None,
            }
        }

        fn required_member_type<'db>(
            db: &'db dyn Db,
            ty: Type<'db>,
            name: &str,
        ) -> Option<Type<'db>> {
            lookup_member(db, ty, name).and_then(|member| {
                (member.definedness == Definedness::AlwaysDefined).then_some(member.ty)
            })
        }

        fn path(prefix: &str, name: &str) -> String {
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}.{name}")
            }
        }

        fn collect_leaves<'db>(
            db: &'db dyn Db,
            expected: Type<'db>,
            prefix: String,
            out: &mut Vec<String>,
        ) {
            let Some(protocol) = chalk_protocol(expected) else {
                out.push(prefix);
                return;
            };

            for member in protocol.interface(db).members(db) {
                let member_path = path(&prefix, member.name());
                if let Some(expected_member) = required_member_type(db, expected, member.name()) {
                    collect_leaves(db, expected_member, member_path, out);
                } else {
                    out.push(member_path);
                }
            }
        }

        fn compare<'db>(
            db: &'db dyn Db,
            expected: Type<'db>,
            mut actual: Type<'db>,
            prefix: &str,
            missing: &mut Vec<String>,
        ) -> bool {
            let Some(protocol) = chalk_protocol(expected) else {
                return actual.is_assignable_to(db, expected);
            };
            if let Type::Intersection(intersection) = actual
                && let Some(supplied) =
                    intersection
                        .positive(db)
                        .iter()
                        .copied()
                        .find(|positive| match positive {
                            Type::ProtocolInstance(protocol) => {
                                protocol.synthesized_kind()
                                    == Some(SynthesizedProtocolKind::ChalkSuppliedFeatures)
                            }
                            _ => false,
                        })
            {
                actual = supplied;
            }

            let mut compatible = true;
            for member in protocol.interface(db).members(db) {
                let member_path = path(prefix, member.name());
                let Some(expected_member) = required_member_type(db, expected, member.name())
                else {
                    compatible = false;
                    continue;
                };

                match lookup_member(db, actual, member.name()) {
                    Some(DefinedPlace {
                        ty: actual_member,
                        definedness: Definedness::AlwaysDefined,
                        ..
                    }) => {
                        compatible &=
                            compare(db, expected_member, actual_member, &member_path, missing);
                    }
                    Some(DefinedPlace {
                        ty: actual_member,
                        definedness: Definedness::PossiblyUndefined,
                        ..
                    }) => {
                        let mut nested_missing = Vec::new();
                        compatible &= compare(
                            db,
                            expected_member,
                            actual_member,
                            &member_path,
                            &mut nested_missing,
                        );
                        collect_leaves(db, expected_member, member_path, missing);
                    }
                    None => collect_leaves(db, expected_member, member_path, missing),
                }
            }
            compatible
        }

        chalk_protocol(self)?;
        let mut missing = Vec::new();
        let compatible = compare(db, self, actual, "", &mut missing);
        (compatible && !missing.is_empty()).then_some(missing)
    }

    pub(super) fn chalk_features_return_is_incomplete(
        self,
        db: &'db dyn Db,
        actual: Type<'db>,
    ) -> bool {
        !self.chalk_features_return_is_compatible(db, actual)
            && self.chalk_missing_return_fields(db, actual).is_some()
    }

    /// Return expressions for `Features[Name.field]` may have the declared type of `Name.field`.
    pub(super) fn chalk_features_return_is_compatible(
        self,
        db: &'db dyn Db,
        actual: Type<'db>,
    ) -> bool {
        let Type::ProtocolInstance(protocol) = self else {
            return false;
        };
        if protocol.synthesized_kind() != Some(SynthesizedProtocolKind::ChalkFeatures) {
            return false;
        }

        fn single_selected_type<'db>(db: &'db dyn Db, ty: Type<'db>) -> Option<Type<'db>> {
            let Type::ProtocolInstance(protocol) = ty else {
                return Some(ty);
            };
            if protocol.synthesized_kind() != Some(SynthesizedProtocolKind::ChalkFeatures) {
                return Some(ty);
            }

            let mut selected = None;
            for member in protocol.interface(db).members(db) {
                let Place::Defined(DefinedPlace {
                    ty: member_ty,
                    definedness: Definedness::AlwaysDefined,
                    ..
                }) = ty.member(db, member.name()).place
                else {
                    return None;
                };
                let member_ty = single_selected_type(db, member_ty)?;
                if selected.replace(member_ty).is_some() {
                    return None;
                }
            }
            selected
        }

        single_selected_type(db, self).is_some_and(|expected| actual.is_assignable_to(db, expected))
    }

    pub(super) fn chalk_instance_member(
        self,
        db: &'db dyn Db,
        name: &str,
    ) -> Option<PlaceAndQualifiers<'db>> {
        let (class, specialization) = self.nominal_class(db)?.static_class_literal(db)?;
        if !class
            .dataclass_params(db)
            .is_some_and(|params| params.flags(db).contains(DataclassFlags::CHALK_FEATURES))
        {
            return None;
        }

        let field_policy = CodeGeneratorKind::from_class(db, class.into())?;
        let field = class.fields(db, specialization, field_policy).get(name)?;
        if !matches!(
            &field.kind,
            FieldKind::Dataclass {
                init_only: false,
                ..
            }
        ) {
            return None;
        }

        Some(Place::bound(field.declared_ty).into())
    }
}
