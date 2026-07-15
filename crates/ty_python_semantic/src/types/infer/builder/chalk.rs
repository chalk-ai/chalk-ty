use ruff_db::parsed::parsed_module;
use ruff_python_ast::{self as ast, AnyNodeRef, name::Name};
use ty_module_resolver::{ModuleName, file_to_module, resolve_module};
use ty_python_core::{definition::DefinitionKind, place_table, scope::ScopeKind, use_def_map};

use crate::{
    FxIndexMap,
    place::{Place, imported_symbol, place_from_declarations},
    reachability::DeclarationsIteratorExtension,
    types::{
        DataclassFlags, MemberLookupPolicy, Type, TypeAndQualifiers, TypeContext, TypeQualifiers,
        infer::{TypeInferenceBuilder, nearest_enclosing_class},
        tuple::TupleType,
    },
};

const CHALK_FEATURES_MODULE: &str = "chalk.features";
const CHALK_STREAMS_MODULE: &str = "chalk.streams";
const CHALK_EXTENSIONS_MODULE: &str = "ty_chalk_extensions";

#[derive(Clone, Copy)]
enum ChalkPathRefinement {
    NonNone,
    None,
}

#[derive(Clone, PartialEq, Eq)]
struct ChalkFeaturePathKey<'db> {
    root: Type<'db>,
    members: Box<[Name]>,
}

#[derive(Clone, Default)]
pub(super) struct ChalkRefinements<'db> {
    paths: Vec<(ChalkFeaturePathKey<'db>, ChalkPathRefinement)>,
}

pub(super) struct ChalkIfThenElseRefinements<'db> {
    if_true: ChalkRefinements<'db>,
    if_false: ChalkRefinements<'db>,
}

struct FeaturePath<'ast> {
    root_expression: ast::ExprRef<'ast>,
    root: &'ast ast::ExprName,
    members: Vec<(ast::ExprRef<'ast>, &'ast ast::Identifier)>,
}

impl<'ast> FeaturePath<'ast> {
    fn from_expression(expression: &'ast ast::Expr) -> Option<Self> {
        Self::from_expression_ref(expression.into())
    }

    fn from_attribute(attribute: &'ast ast::ExprAttribute) -> Option<Self> {
        Self::from_expression_ref(attribute.into())
    }

    fn from_expression_ref(expression: ast::ExprRef<'ast>) -> Option<Self> {
        let mut current = expression;
        let mut members = Vec::new();

        while let ast::ExprRef::Attribute(attribute) = current {
            members.push((current, &attribute.attr));
            current = (&attribute.value).into();
        }

        let ast::ExprRef::Name(root) = current else {
            return None;
        };
        if members.is_empty() {
            return None;
        }

        members.reverse();
        Some(Self {
            root_expression: current,
            root,
            members,
        })
    }
}

struct ResolvedFeaturePath<'db> {
    members: Vec<(Name, Type<'db>)>,
}

#[derive(Default)]
struct FeatureShape<'db> {
    selected: Option<Type<'db>>,
    children: FxIndexMap<Name, FeatureShape<'db>>,
}

impl<'db> FeatureShape<'db> {
    fn member_type(self, db: &'db dyn crate::Db) -> Type<'db> {
        self.selected.unwrap_or_else(|| self.into_protocol_type(db))
    }

    fn into_protocol_type(self, db: &'db dyn crate::Db) -> Type<'db> {
        let members: Vec<_> = self
            .children
            .into_iter()
            .map(|(name, shape)| (name, shape.member_type(db)))
            .collect();
        Type::chalk_features_protocol(db, members.iter().map(|(name, ty)| (name.as_str(), *ty)))
    }
}

impl<'db> TypeInferenceBuilder<'db, '_> {
    fn chalk_feature_path_key(
        &self,
        path: &FeaturePath<'_>,
        root_ty: Type<'db>,
        member_count: usize,
    ) -> Option<ChalkFeaturePathKey<'db>> {
        let root = if self.is_chalk_features_class(root_ty) {
            root_ty
        } else if self.is_chalk_features_symbol(root_ty, "_") && self.in_chalk_features_class() {
            Type::ClassLiteral(nearest_enclosing_class(self.db(), self.index, self.scope())?.into())
        } else {
            return None;
        };

        Some(ChalkFeaturePathKey {
            root,
            members: path
                .members
                .iter()
                .take(member_count)
                .map(|(_, name)| name.id.clone())
                .collect(),
        })
    }

    fn chalk_feature_path_key_from_expression(
        &self,
        expression: &ast::Expr,
    ) -> Option<ChalkFeaturePathKey<'db>> {
        let path = FeaturePath::from_expression(expression)?;
        let mut speculative = self.speculate_without_diagnostics();
        let root_ty = speculative.infer_name_expression(path.root);
        speculative.chalk_feature_path_key(&path, root_ty, path.members.len())
    }

    fn collect_chalk_condition_refinements(
        &self,
        expression: &ast::Expr,
        truthy: bool,
        refinements: &mut ChalkRefinements<'db>,
    ) {
        match expression {
            ast::Expr::Compare(compare) => {
                let ([op], [comparator]) = (&*compare.ops, &*compare.comparators) else {
                    return;
                };
                let path_expression = if comparator.is_none_literal_expr() {
                    &*compare.left
                } else if compare.left.is_none_literal_expr() {
                    comparator
                } else {
                    return;
                };
                let refinement = match (op, truthy) {
                    (ast::CmpOp::NotEq | ast::CmpOp::IsNot, true)
                    | (ast::CmpOp::Eq | ast::CmpOp::Is, false) => ChalkPathRefinement::NonNone,
                    (ast::CmpOp::Eq | ast::CmpOp::Is, true)
                    | (ast::CmpOp::NotEq | ast::CmpOp::IsNot, false) => ChalkPathRefinement::None,
                    _ => return,
                };
                if let Some(path) = self.chalk_feature_path_key_from_expression(path_expression) {
                    refinements.paths.push((path, refinement));
                }
            }
            ast::Expr::BinOp(binary)
                if (truthy && binary.op == ast::Operator::BitAnd)
                    || (!truthy && binary.op == ast::Operator::BitOr) =>
            {
                self.collect_chalk_condition_refinements(&binary.left, truthy, refinements);
                self.collect_chalk_condition_refinements(&binary.right, truthy, refinements);
            }
            ast::Expr::BoolOp(boolean)
                if (truthy && boolean.op == ast::BoolOp::And)
                    || (!truthy && boolean.op == ast::BoolOp::Or) =>
            {
                for value in &boolean.values {
                    self.collect_chalk_condition_refinements(value, truthy, refinements);
                }
            }
            ast::Expr::UnaryOp(unary)
                if matches!(unary.op, ast::UnaryOp::Not | ast::UnaryOp::Invert) =>
            {
                self.collect_chalk_condition_refinements(&unary.operand, !truthy, refinements);
            }
            _ => {}
        }
    }

    pub(super) fn chalk_if_then_else_refinements(
        &self,
        callable_type: Type<'db>,
        arguments: &ast::Arguments,
    ) -> Option<ChalkIfThenElseRefinements<'db>> {
        let function = match callable_type {
            Type::FunctionLiteral(function) => function,
            Type::BoundMethod(method) => method.function(self.db()),
            _ => return None,
        };
        if function.name(self.db()) != "if_then_else" {
            return None;
        }
        let module = file_to_module(self.db(), function.file(self.db()))?;
        if !matches!(
            module.name(self.db()).as_str(),
            "chalk.functions" | "chalk.features.underscore"
        ) || !arguments.keywords.is_empty()
            || arguments.args.iter().any(ast::Expr::is_starred_expr)
        {
            return None;
        }
        let [condition, _, _] = &*arguments.args else {
            return None;
        };

        let mut if_true = ChalkRefinements::default();
        let mut if_false = ChalkRefinements::default();
        self.collect_chalk_condition_refinements(condition, true, &mut if_true);
        self.collect_chalk_condition_refinements(condition, false, &mut if_false);
        Some(ChalkIfThenElseRefinements { if_true, if_false })
    }

    pub(super) fn infer_chalk_if_then_else_argument<T>(
        &mut self,
        refinements: Option<&ChalkIfThenElseRefinements<'db>>,
        argument_index: usize,
        infer: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let Some(refinements) = refinements.and_then(|refinements| match argument_index {
            1 => Some(&refinements.if_true),
            2 => Some(&refinements.if_false),
            _ => None,
        }) else {
            return infer(self);
        };

        let original_len = self.chalk_refinements.paths.len();
        self.chalk_refinements
            .paths
            .extend(refinements.paths.iter().cloned());
        let result = infer(self);
        self.chalk_refinements.paths.truncate(original_len);
        result
    }

    fn refine_chalk_feature_path(
        &self,
        path: &FeaturePath<'_>,
        root_ty: Type<'db>,
        member_count: usize,
        mut ty: Type<'db>,
    ) -> Type<'db> {
        let Some(key) = self.chalk_feature_path_key(path, root_ty, member_count) else {
            return ty;
        };
        for (_, refinement) in self
            .chalk_refinements
            .paths
            .iter()
            .filter(|(path, _)| path == &key)
        {
            ty = ty.filter_union(self.db(), |element| match refinement {
                ChalkPathRefinement::NonNone => !element.is_none(self.db()),
                ChalkPathRefinement::None => element.is_none(self.db()),
            });
        }
        ty
    }

    #[track_caller]
    fn store_chalk_expression_type(&mut self, expression: ast::ExprRef<'_>, ty: Type<'db>) {
        let previous = self.expressions.insert(expression.into(), ty);
        assert_eq!(previous, None);
    }

    fn chalk_extension_class(&self, name: &str) -> Option<crate::types::StaticClassLiteral<'db>> {
        let module_name = ModuleName::new_static(CHALK_EXTENSIONS_MODULE)?;
        let module = resolve_module(self.db(), self.file(), &module_name)?;
        let Type::ClassLiteral(class) =
            imported_symbol(self.db(), module.file(self.db()), name, None)
                .ignore_possibly_undefined()?
        else {
            return None;
        };
        class.as_static()
    }

    fn chalk_symbolic_feature_type(&self, inner: Type<'db>) -> Type<'db> {
        let (class_name, inner) = self
            .chalk_windowed_inner(inner)
            .map_or(("Resolved", inner), |inner| ("Windowed", inner));
        let Some(class) = self.chalk_extension_class(class_name) else {
            return Type::unknown();
        };
        Type::instance(
            self.db(),
            class.apply_specialization(self.db(), |generic_context| {
                generic_context.specialize(self.db(), &[inner])
            }),
        )
    }

    fn chalk_symbolic_inner(&self, ty: Type<'db>) -> Option<Type<'db>> {
        ["Resolved", "Windowed"].into_iter().find_map(|name| {
            ty.specialization_of(self.db(), self.chalk_extension_class(name)?)?
                .types(self.db())
                .first()
                .copied()
        })
    }

    fn chalk_windowed_inner(&self, ty: Type<'db>) -> Option<Type<'db>> {
        let module_name = ModuleName::new_static(CHALK_STREAMS_MODULE)?;
        let module = resolve_module(self.db(), self.file(), &module_name)?;
        let Type::ClassLiteral(class) =
            imported_symbol(self.db(), module.file(self.db()), "Windowed", None)
                .ignore_possibly_undefined()?
        else {
            return None;
        };
        ty.specialization_of(self.db(), class.as_static()?)?
            .types(self.db())
            .first()
            .copied()
    }

    pub(super) fn chalk_feature_initializer_assignment(
        &self,
        node: AnyNodeRef<'_>,
        declared: &TypeAndQualifiers<'db>,
        inferred: Type<'db>,
    ) -> (Type<'db>, Type<'db>, bool) {
        let is_chalk_feature = self.in_chalk_features_class()
            && matches!(node, AnyNodeRef::ExprName(_))
            && !declared.qualifiers.contains(TypeQualifiers::CLASS_VAR);
        let declared = if is_chalk_feature {
            declared
                .inner_type()
                .chalk_feature_value_type(self.db(), self.file())
        } else {
            declared.inner_type()
        };
        if is_chalk_feature && let Some(inner) = self.chalk_symbolic_inner(inferred) {
            (declared, inner, true)
        } else {
            (declared, inferred, false)
        }
    }

    fn chalk_features_symbol(&self, name: &str) -> Option<Type<'db>> {
        let module_name = ModuleName::new_static(CHALK_FEATURES_MODULE)?;
        let module = resolve_module(self.db(), self.file(), &module_name)?;
        imported_symbol(self.db(), module.file(self.db()), name, None).ignore_possibly_undefined()
    }

    fn is_chalk_features_symbol(&self, ty: Type<'db>, name: &str) -> bool {
        self.chalk_features_symbol(name) == Some(ty)
    }

    fn is_chalk_features_class(&self, ty: Type<'db>) -> bool {
        let Type::ClassLiteral(class) = ty else {
            return false;
        };
        class
            .as_static()
            .and_then(|class| class.dataclass_params(self.db()))
            .is_some_and(|params| {
                params
                    .flags(self.db())
                    .contains(DataclassFlags::CHALK_FEATURES)
            })
    }

    pub(super) fn in_chalk_features_class(&self) -> bool {
        self.index
            .scope(self.scope().file_scope_id(self.db()))
            .kind()
            == ScopeKind::Class
            && nearest_enclosing_class(self.db(), self.index, self.scope()).is_some_and(|class| {
                class.dataclass_params(self.db()).is_some_and(|params| {
                    params
                        .flags(self.db())
                        .contains(DataclassFlags::CHALK_FEATURES)
                })
            })
    }

    pub(super) fn is_chalk_features_decorator(
        &self,
        decorator_ty: Type<'db>,
        decorator_call_ty: Option<Type<'db>>,
    ) -> bool {
        self.is_chalk_features_symbol(decorator_ty, "features")
            || decorator_call_ty.is_some_and(|ty| self.is_chalk_features_symbol(ty, "features"))
    }

    fn chalk_feature_member(&mut self, ty: Type<'db>, name: &str) -> Option<Type<'db>> {
        let (class, specialization) = ty
            .nominal_class(self.db())?
            .static_class_literal(self.db())?;
        if !class.dataclass_params(self.db()).is_some_and(|params| {
            params
                .flags(self.db())
                .contains(DataclassFlags::CHALK_FEATURES)
        }) {
            return None;
        }

        let body_scope = class.body_scope(self.db());
        let table = place_table(self.db(), body_scope);
        if let Some(symbol_id) = table.symbol_id(name) {
            let declarations =
                use_def_map(self.db(), body_scope).end_of_scope_symbol_declarations(symbol_id);
            if declarations
                .clone()
                .any_reachable(self.db(), |declaration| {
                    declaration.is_defined_and(|declaration| {
                        !matches!(
                            declaration.kind(self.db()),
                            DefinitionKind::AnnotatedAssignment(..)
                        )
                    })
                })
            {
                return None;
            }
            let declared =
                place_from_declarations(self.db(), declarations).ignore_conflicting_declarations();
            if declared.is_class_var() || declared.is_init_var() {
                return None;
            }
            if let Place::Defined(place) = declared.place {
                return Some(
                    place
                        .ty
                        .apply_optional_specialization(self.db(), specialization)
                        .chalk_feature_value_type(self.db(), self.file()),
                );
            }
        }

        if let Some(member) = ty
            .instance_member(self.db(), name)
            .ignore_possibly_undefined()
        {
            return Some(member);
        }

        // The annotation expressions below must be inferred in their owning class-body scope.
        // This fallback is specifically for `_` expressions in that surrounding feature class.
        if body_scope != self.scope() {
            return None;
        }

        let module = parsed_module(self.db(), class.file(self.db())).load(self.db());
        for (_, declarations) in
            use_def_map(self.db(), body_scope).all_end_of_scope_symbol_declarations()
        {
            let Some(assignment) = declarations
                .filter_map(|declaration| {
                    let definition = declaration.declaration.definition()?;
                    let DefinitionKind::AnnotatedAssignment(assignment) =
                        definition.kind(self.db())
                    else {
                        return None;
                    };
                    Some(assignment)
                })
                .next()
            else {
                continue;
            };
            let (ast::Expr::Subscript(annotation), Some(ast::Expr::Call(value))) =
                (assignment.annotation(&module), assignment.value(&module))
            else {
                continue;
            };

            let mut speculative = self.speculate_without_diagnostics();
            let dataframe_ty =
                speculative.infer_expression(&annotation.value, TypeContext::default());
            let has_many_ty = speculative.infer_expression(&value.func, TypeContext::default());
            if !speculative.is_chalk_features_symbol(dataframe_ty, "DataFrame")
                || !speculative.is_chalk_features_symbol(has_many_ty, "has_many")
            {
                continue;
            }
            let row_ty = speculative.infer_type_expression(&annotation.slice);
            let row_is_features_class = row_ty
                .nominal_class(self.db())
                .and_then(|class| class.static_class_literal(self.db()))
                .and_then(|(class, _)| class.dataclass_params(self.db()))
                .is_some_and(|params| {
                    params
                        .flags(self.db())
                        .contains(DataclassFlags::CHALK_FEATURES)
                });
            if row_is_features_class
                && let Some(member) = row_ty
                    .instance_member(self.db(), name)
                    .ignore_possibly_undefined()
            {
                return Some(member);
            }
        }

        None
    }

    fn resolve_chalk_feature_path_from(
        &mut self,
        path: &FeaturePath<'_>,
        root_ty: Type<'db>,
        mut current: Type<'db>,
        store_terminal: bool,
        resolved_values: bool,
        diagnose_missing: bool,
    ) -> Option<ResolvedFeaturePath<'db>> {
        self.store_chalk_expression_type(path.root_expression, root_ty);

        let mut members = Vec::with_capacity(path.members.len());
        let last_member = path.members.len() - 1;

        for (index, (expression, name)) in path.members.iter().enumerate() {
            let Some(member_ty) = self.chalk_feature_member(current, name.id.as_str()) else {
                if diagnose_missing {
                    let ast::ExprRef::Attribute(attribute) = *expression else {
                        unreachable!();
                    };
                    let ty = self.infer_attribute_load_impl(attribute, current);
                    if index != last_member {
                        self.store_chalk_expression_type(*expression, ty);
                    }
                    members.push((name.id.clone(), ty));
                    return Some(ResolvedFeaturePath { members });
                }
                return None;
            };
            current = member_ty;
            current = self.refine_chalk_feature_path(path, root_ty, index + 1, current);
            members.push((name.id.clone(), current));

            if store_terminal || index != last_member {
                self.store_chalk_expression_type(
                    *expression,
                    if resolved_values {
                        self.chalk_symbolic_feature_type(current)
                    } else {
                        current
                    },
                );
            }
        }

        Some(ResolvedFeaturePath { members })
    }

    pub(super) fn infer_chalk_feature_path_type_expression(
        &mut self,
        expression: &ast::Expr,
    ) -> Option<Type<'db>> {
        let path = FeaturePath::from_expression(expression)?;
        let mut speculative = self.speculate_without_diagnostics();
        let root_ty = speculative.infer_name_expression(path.root);
        let Type::ClassLiteral(root_class) = root_ty else {
            return None;
        };
        if !speculative.is_chalk_features_class(root_ty) {
            return None;
        }
        let current = Type::instance(
            speculative.db(),
            root_class.default_specialization(speculative.db()),
        );
        speculative
            .resolve_chalk_feature_path_from(&path, root_ty, current, false, false, false)?;

        let root_ty = self.infer_name_expression(path.root);
        let Type::ClassLiteral(root_class) = root_ty else {
            return None;
        };
        let current = Type::instance(self.db(), root_class.default_specialization(self.db()));
        self.resolve_chalk_feature_path_from(&path, root_ty, current, false, false, false)?
            .members
            .last()
            .map(|(_, ty)| *ty)
    }

    pub(super) fn infer_chalk_feature_path_value_expression(
        &mut self,
        attribute: &ast::ExprAttribute,
    ) -> Option<Type<'db>> {
        let path = FeaturePath::from_attribute(attribute)?;
        let mut speculative = self.speculate_without_diagnostics();
        let root_ty = speculative.infer_name_expression(path.root);
        let (current, underscore_root) = if speculative.is_chalk_features_class(root_ty) {
            let Type::ClassLiteral(root_class) = root_ty else {
                unreachable!();
            };
            (
                Type::instance(
                    speculative.db(),
                    root_class.default_specialization(speculative.db()),
                ),
                false,
            )
        } else if speculative.is_chalk_features_symbol(root_ty, "_")
            && speculative.in_chalk_features_class()
        {
            let first_member = &path.members[0].1.id;
            if !root_ty
                .member_lookup_with_policy(
                    speculative.db(),
                    first_member.clone(),
                    MemberLookupPolicy::NO_GETATTR_LOOKUP,
                )
                .place
                .is_undefined()
            {
                return None;
            }
            let class =
                nearest_enclosing_class(speculative.db(), speculative.index, speculative.scope())?;
            (
                Type::instance(
                    speculative.db(),
                    class.default_specialization(speculative.db()),
                ),
                true,
            )
        } else {
            return None;
        };
        if !underscore_root {
            speculative
                .resolve_chalk_feature_path_from(&path, root_ty, current, true, true, false)?;
        }

        let root_ty = self.infer_name_expression(path.root);
        let current = if self.is_chalk_features_class(root_ty) {
            let Type::ClassLiteral(root_class) = root_ty else {
                unreachable!();
            };
            Type::instance(self.db(), root_class.default_specialization(self.db()))
        } else {
            current
        };
        let resolved = self.resolve_chalk_feature_path_from(
            &path,
            root_ty,
            current,
            false,
            true,
            underscore_root,
        )?;
        let terminal = resolved.members.last()?.1;
        if terminal.is_dynamic() {
            Some(terminal)
        } else {
            Some(self.chalk_symbolic_feature_type(terminal))
        }
    }

    pub(super) fn infer_chalk_features_type_expression(
        &mut self,
        slice: &ast::Expr,
        value_ty: Type<'db>,
    ) -> Option<Type<'db>> {
        if !self.is_chalk_features_symbol(value_ty, "Features") {
            return None;
        }

        let expressions: Vec<&ast::Expr> = match slice {
            ast::Expr::Tuple(tuple) => tuple.elts.iter().collect(),
            expression => vec![expression],
        };
        if expressions.is_empty() {
            return None;
        }

        let paths: Option<Vec<_>> = expressions
            .iter()
            .map(|expression| FeaturePath::from_expression(expression))
            .collect();
        let paths = paths?;

        let mut speculative = self.speculate_without_diagnostics();
        for path in &paths {
            let root_ty = speculative.infer_name_expression(path.root);
            let Type::ClassLiteral(root_class) = root_ty else {
                return None;
            };
            if !speculative.is_chalk_features_class(root_ty) {
                return None;
            }
            let current = Type::instance(
                speculative.db(),
                root_class.default_specialization(speculative.db()),
            );
            speculative
                .resolve_chalk_feature_path_from(path, root_ty, current, true, false, false)?;
        }

        let mut shape = FeatureShape::default();
        let mut terminal_types = Vec::with_capacity(paths.len());
        for path in &paths {
            let root_ty = self.infer_name_expression(path.root);
            let Type::ClassLiteral(root_class) = root_ty else {
                return None;
            };
            let current = Type::instance(self.db(), root_class.default_specialization(self.db()));
            let resolved =
                self.resolve_chalk_feature_path_from(path, root_ty, current, true, false, false)?;
            let Some((_, terminal_ty)) = resolved.members.last() else {
                return None;
            };
            terminal_types.push(*terminal_ty);

            let member_count = resolved.members.len();
            let mut current_shape = &mut shape;
            for (index, (name, ty)) in resolved.members.into_iter().enumerate() {
                current_shape = current_shape.children.entry(name).or_default();
                if index + 1 == member_count {
                    current_shape.selected = Some(ty);
                }
            }
        }

        if slice.is_tuple_expr() {
            self.store_expression_type(
                slice,
                Type::tuple(TupleType::heterogeneous(self.db(), terminal_types)),
            );
        }

        Some(shape.into_protocol_type(self.db()))
    }
}
