use super::ConstructorBinding;
use crate::db::Db;
use crate::types::class::{CodeGeneratorKind, FieldKind};
use crate::types::{ClassLiteral, DataclassFlags, IntersectionBuilder, Type};

impl<'db> ConstructorBinding<'db> {
    pub(super) fn refine_chalk_features_return_type(
        &self,
        db: &'db dyn Db,
        return_ty: Type<'db>,
    ) -> Type<'db> {
        let Some(class) = self
            .constructed_class_literal(db)
            .and_then(ClassLiteral::as_static)
        else {
            return return_ty;
        };
        if !class
            .dataclass_params(db)
            .is_some_and(|params| params.flags(db).contains(DataclassFlags::CHALK_FEATURES))
        {
            return return_ty;
        }

        let Some(CodeGeneratorKind::DataclassLike(transformer_params)) =
            CodeGeneratorKind::from_class(db, class.into())
        else {
            return return_ty;
        };
        let field_policy = CodeGeneratorKind::DataclassLike(transformer_params);
        let specialization = return_ty
            .class_specialization(db)
            .map(|(_, specialization)| specialization);
        let fields = class.fields(db, specialization, field_policy);

        let [overload] = self.callable().overloads() else {
            return return_ty;
        };
        let recover_from_call_error = !overload.errors().is_empty();

        let mut members = Vec::new();
        for (index, (parameter, parameter_ty)) in overload
            .signature
            .parameters()
            .into_iter()
            .zip(overload.parameter_types())
            .enumerate()
        {
            let Some(parameter_ty) = parameter_ty else {
                continue;
            };
            if !overload.argument_matches().iter().any(|argument| {
                argument.matched
                    && argument.parameters.iter().any(|matched_parameter| {
                        matched_parameter.index == index && matched_parameter.definitely_supplied
                    })
            }) {
                continue;
            }

            let Some(name) = parameter.name() else {
                continue;
            };
            let Some(field) = fields.get(name) else {
                continue;
            };
            if !matches!(
                &field.kind,
                FieldKind::Dataclass {
                    init_only: false,
                    init: true,
                    ..
                }
            ) {
                continue;
            }

            members.push((
                name.clone(),
                if recover_from_call_error {
                    Type::unknown()
                } else {
                    *parameter_ty
                },
            ));
        }

        if members.is_empty() {
            return return_ty;
        }

        let supplied_fields = Type::protocol_with_readonly_members(
            db,
            members.iter().map(|(name, ty)| (name.as_str(), *ty)),
        );
        IntersectionBuilder::new(db)
            .add_positive(return_ty)
            .add_positive(supplied_fields)
            .build()
    }
}
