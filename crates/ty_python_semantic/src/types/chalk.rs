use super::class::{CodeGeneratorKind, FieldKind};
use super::{DataclassFlags, Type};
use crate::db::Db;
use crate::place::PlaceAndQualifiers;

impl<'db> Type<'db> {
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

        Some(self.instance_member(db, name))
    }
}
