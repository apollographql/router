use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::ExtendedType;

use crate::error::SubgraphLocation;
use crate::merger::hints::DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE;
use crate::merger::merge::Merger;
use crate::schema::fed3_upgrader::StrippedDeprecation;
use crate::supergraph::CompositionHint;

enum Resolution {
    /// A merged interface field is not deprecated, so the implementation must not be.
    Strip(CompositionHint),
    /// Every merged interface field is deprecated, so the `@deprecated` the upgrade removed is
    /// put back.
    Restore(Node<Directive>),
}

impl Merger {
    /// Enforces the 2025 spec rule that an implementing field may only be deprecated when every
    /// interface field it implements is deprecated. The fed3 upgrade applies the rule against each
    /// subgraph's own copy of an interface, which can disagree with the merged interface in both
    /// directions, so the merged schema has the final say: `@deprecated` is removed where a merged
    /// interface field is not deprecated, and restored where the upgrade removed it needlessly.
    pub(in crate::merger) fn merge_implementation_deprecations(&mut self) {
        let mut stripped_by_field: IndexMap<(&Name, &Name), Vec<(&str, &StrippedDeprecation)>> =
            IndexMap::default();
        for subgraph in &self.subgraphs {
            for stripped in subgraph.stripped_deprecations() {
                stripped_by_field
                    .entry((&stripped.type_name, &stripped.field_name))
                    .or_default()
                    .push((&subgraph.name, stripped));
            }
        }

        let schema = self.merged.schema();
        let mut resolutions: Vec<(Name, Name, Resolution)> = Vec::new();
        for (type_name, ty) in &schema.types {
            let (implements, fields) = match ty {
                ExtendedType::Object(object) => (&object.implements_interfaces, &object.fields),
                ExtendedType::Interface(interface) => {
                    (&interface.implements_interfaces, &interface.fields)
                }
                _ => continue,
            };
            for (field_name, field) in fields {
                let declaring = implements
                    .iter()
                    .filter_map(|iface| {
                        Some((&**iface, interface_field(schema, iface, field_name)?))
                    })
                    .collect::<Vec<_>>();
                let stripped = stripped_by_field
                    .get(&(type_name, field_name))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                if let Some(resolution) =
                    self.resolve_field_deprecation(type_name, field, &declaring, stripped)
                {
                    resolutions.push((type_name.clone(), field_name.clone(), resolution));
                }
            }
        }

        for (type_name, field_name, resolution) in resolutions {
            let directives =
                field_directives_mut(self.merged.schema_mut(), &type_name, &field_name);
            match resolution {
                Resolution::Strip(hint) => {
                    if let Some(directives) = directives {
                        directives.0.retain(|d| d.name != "deprecated");
                    }
                    self.error_reporter.add_hint(hint);
                }
                Resolution::Restore(directive) => {
                    if let Some(directives) = directives {
                        directives.0.push(directive);
                    }
                }
            }
        }
    }

    /// Decides the deprecation of one implementing field from the merged interface fields it
    /// implements (`declaring`) and any `@deprecated` the fed3 upgrade removed from it.
    fn resolve_field_deprecation(
        &self,
        type_name: &Name,
        field: &FieldDefinition,
        declaring: &[(&Name, &FieldDefinition)],
        stripped: &[(&str, &StrippedDeprecation)],
    ) -> Option<Resolution> {
        let field_name = &field.name;
        let is_deprecated = field.directives.has("deprecated");
        let Some((iface, _)) = declaring
            .iter()
            .find(|(_, iface_field)| !iface_field.directives.has("deprecated"))
        else {
            if declaring.is_empty() || is_deprecated {
                return None;
            }
            let (_, first) = stripped.first()?;
            return Some(Resolution::Restore(first.directive.clone()));
        };

        let mut locations = self.deprecated_locations(type_name, field_name);
        locations.extend(
            stripped
                .iter()
                .map(|(subgraph, stripped)| SubgraphLocation {
                    subgraph: (*subgraph).to_owned(),
                    range: stripped.range.clone(),
                }),
        );
        if locations.is_empty() {
            return None;
        }
        Some(Resolution::Strip(CompositionHint {
            definition: &DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE,
            message: format!(
                "Stripped `@deprecated` from `{type_name}.{field_name}` because \
                 `{iface}.{field_name}` is not deprecated. Either deprecate \
                 the interface field or remove `@deprecated` from the implementation."
            ),
            locations,
        }))
    }

    /// Where `type_name.field_name` is still deprecated in a subgraph schema.
    fn deprecated_locations(&self, type_name: &Name, field_name: &Name) -> Vec<SubgraphLocation> {
        self.subgraphs
            .iter()
            .filter_map(|subgraph| {
                let schema = subgraph.schema().schema();
                let directive = schema
                    .type_field(type_name, field_name)
                    .ok()?
                    .directives
                    .get("deprecated")?;
                Some(SubgraphLocation {
                    subgraph: subgraph.name.clone(),
                    range: directive.line_column_range(&schema.sources)?,
                })
            })
            .collect()
    }
}

fn interface_field<'a>(
    schema: &'a Schema,
    interface_name: &Name,
    field_name: &Name,
) -> Option<&'a FieldDefinition> {
    match schema.types.get(interface_name)? {
        ExtendedType::Interface(interface) => interface.fields.get(field_name).map(|f| &**f),
        _ => None,
    }
}

fn field_directives_mut<'a>(
    schema: &'a mut Schema,
    type_name: &Name,
    field_name: &Name,
) -> Option<&'a mut DirectiveList> {
    let fields = match schema.types.get_mut(type_name)? {
        ExtendedType::Object(object) => &mut object.make_mut().fields,
        ExtendedType::Interface(interface) => &mut interface.make_mut().fields,
        _ => return None,
    };
    Some(&mut fields.get_mut(field_name)?.make_mut().directives)
}
