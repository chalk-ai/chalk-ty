use super::class::{CodeGeneratorKind, FieldKind};
use super::instance::SynthesizedProtocolKind;
use super::{DataclassFlags, Type};
use crate::db::Db;
use crate::place::{DefinedPlace, Definedness, Place, PlaceAndQualifiers};

impl<'db> Type<'db> {
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
            actual: Type<'db>,
            prefix: &str,
            missing: &mut Vec<String>,
        ) -> bool {
            let Some(protocol) = chalk_protocol(expected) else {
                return actual.is_assignable_to(db, expected);
            };

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
