use ty_python_semantic::chalk::chalk_feature_path_member;
use ty_python_semantic::{DisplaySettings, SemanticModel};

use super::{GotoTarget, HoverContent};

pub(super) fn feature_path_hover<'db>(
    model: &SemanticModel<'db>,
    target: &GotoTarget<'_>,
) -> Option<HoverContent<'db>> {
    let GotoTarget::Expression(ruff_python_ast::ExprRef::Attribute(attribute)) = target else {
        return None;
    };
    let feature = chalk_feature_path_member(model, attribute)?;
    let declared_ty = feature
        .declared_ty
        .display_with(model.db(), DisplaySettings::default().multiline());
    Some(HoverContent::Signature(format!(
        "(feature) {}.{}: {declared_ty}",
        feature.owner, attribute.attr
    )))
}
