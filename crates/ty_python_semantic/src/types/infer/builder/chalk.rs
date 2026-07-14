use ruff_python_ast::{self as ast, name::Name};
use ty_module_resolver::{ModuleName, resolve_module};

use crate::{
    FxIndexMap,
    place::imported_symbol,
    types::{DataclassFlags, Type, infer::TypeInferenceBuilder, tuple::TupleType},
};

const CHALK_FEATURES_MODULE: &str = "chalk.features";

struct FeaturePath<'ast> {
    root_expression: &'ast ast::Expr,
    root: &'ast ast::ExprName,
    members: Vec<(&'ast ast::Expr, &'ast ast::Identifier)>,
}

impl<'ast> FeaturePath<'ast> {
    fn from_expression(expression: &'ast ast::Expr) -> Option<Self> {
        let mut current = expression;
        let mut members = Vec::new();

        while let ast::Expr::Attribute(attribute) = current {
            members.push((current, &attribute.attr));
            current = &attribute.value;
        }

        let ast::Expr::Name(root) = current else {
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
        Type::protocol_with_readonly_members(
            db,
            members.iter().map(|(name, ty)| (name.as_str(), *ty)),
        )
    }
}

impl<'db> TypeInferenceBuilder<'db, '_> {
    fn chalk_features_symbol(&self, name: &str) -> Option<Type<'db>> {
        let module_name = ModuleName::new_static(CHALK_FEATURES_MODULE)?;
        let module = resolve_module(self.db(), self.file(), &module_name)?;
        imported_symbol(self.db(), module.file(self.db()), name, None).ignore_possibly_undefined()
    }

    fn is_chalk_features_symbol(&self, ty: Type<'db>, name: &str) -> bool {
        self.chalk_features_symbol(name) == Some(ty)
    }

    pub(super) fn is_chalk_features_decorator(
        &self,
        decorator_ty: Type<'db>,
        decorator_call_ty: Option<Type<'db>>,
    ) -> bool {
        self.is_chalk_features_symbol(decorator_ty, "features")
            || decorator_call_ty.is_some_and(|ty| self.is_chalk_features_symbol(ty, "features"))
    }

    fn resolve_chalk_feature_path(
        &mut self,
        path: &FeaturePath<'_>,
        store_terminal: bool,
    ) -> Option<ResolvedFeaturePath<'db>> {
        let root_ty = self.infer_name_expression(path.root);
        let Type::ClassLiteral(root_class) = root_ty else {
            return None;
        };
        if !root_class
            .as_static()
            .and_then(|class| class.dataclass_params(self.db()))
            .is_some_and(|params| {
                params
                    .flags(self.db())
                    .contains(DataclassFlags::CHALK_FEATURES)
            })
        {
            return None;
        }

        self.store_expression_type(path.root_expression, root_ty);

        let mut current = Type::instance(self.db(), root_class.default_specialization(self.db()));
        let mut members = Vec::with_capacity(path.members.len());
        let last_member = path.members.len() - 1;

        for (index, (expression, name)) in path.members.iter().enumerate() {
            current = current
                .instance_member(self.db(), name.id.as_str())
                .ignore_possibly_undefined()?;
            members.push((name.id.clone(), current));

            if store_terminal || index != last_member {
                self.store_expression_type(expression, current);
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
        speculative.resolve_chalk_feature_path(&path, false)?;

        self.resolve_chalk_feature_path(&path, false)?
            .members
            .last()
            .map(|(_, ty)| *ty)
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
            speculative.resolve_chalk_feature_path(path, true)?;
        }

        let mut shape = FeatureShape::default();
        let mut terminal_types = Vec::with_capacity(paths.len());
        for path in &paths {
            let resolved = self.resolve_chalk_feature_path(path, true)?;
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
