//! Runtime dispatch mapping.
//!
//! [`schema::generate`](crate::schema::generate) renders a GraphQL SDL from a component's WIT.
//! The router needs the *inverse* of the naming it applies: given a GraphQL field name from an
//! operation (e.g. `incidentsV2Show`), which WIT export does it invoke, and how do the GraphQL
//! arguments map back onto the WIT function's parameters? [`operation_map`] returns exactly that,
//! computed from the SAME traversal and naming rules the SDL generator uses, so the two can never
//! drift.

use wit_parser::{Function, InterfaceId, Resolve, WorldId, WorldItem, WorldKey};

use crate::naming::to_camel_case;
use crate::schema::{FieldKind, classify};

/// Whether a field lives under the GraphQL `Query` or `Mutation` root — mirrors
/// [`schema`](crate::schema)'s verb-prefix classification.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum OpKind {
    Query,
    Mutation,
}

/// Where an exported function lives inside the component, for the wasmtime dynamic component API.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ExportLocation {
    /// Exported directly on the component/world root: `Instance::get_func(<func>)`.
    Root,
    /// Exported inside an interface. `instance_name` is the fully-qualified component-model
    /// export name to navigate to (e.g. `incidentio:api/incidents-v2@0.1.0`); the function is
    /// then looked up within that instance by its WIT (kebab) name.
    Interface { instance_name: String },
}

/// One GraphQL argument's correspondence to a WIT function parameter. GraphQL arg names are
/// camelCase (as emitted into the SDL); WIT parameter names are kebab-case.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParamMapping {
    /// camelCase argument name as it appears in the GraphQL operation.
    pub graphql_arg: String,
    /// kebab-case WIT parameter name.
    pub wit_param: String,
}

/// How a single GraphQL field resolves to a WIT export.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FieldMapping {
    /// camelCase GraphQL field name, e.g. `incidentsV2Show` or `getUser`.
    pub graphql_field: String,
    /// `Query` or `Mutation`.
    pub kind: OpKind,
    /// Where the backing function is exported.
    pub export: ExportLocation,
    /// The WIT (kebab) function name to invoke, e.g. `show`.
    pub func: String,
    /// GraphQL argument ↔ WIT parameter correspondence, in declaration order.
    pub params: Vec<ParamMapping>,
}

/// The complete field→export mapping for a component's world.
#[derive(Clone, Debug, Default)]
pub struct OperationMap {
    pub fields: Vec<FieldMapping>,
}

impl OperationMap {
    /// Look up a mapping by its GraphQL field name.
    pub fn get(&self, graphql_field: &str) -> Option<&FieldMapping> {
        self.fields.iter().find(|f| f.graphql_field == graphql_field)
    }
}

/// Build the [`OperationMap`] for a decoded component's world.
///
/// This walks the world's exports the same way [`schema`](crate::schema) does — world-level
/// functions and each exported interface's functions — so the GraphQL field names produced here
/// are identical to those in the generated SDL.
pub fn operation_map(resolve: &Resolve, world: WorldId) -> OperationMap {
    let mut fields = Vec::new();
    let world = &resolve.worlds[world];

    for (key, item) in world.exports.iter() {
        match item {
            WorldItem::Function(func) => {
                let func_kebab = match key {
                    WorldKey::Name(n) => n.clone(),
                    WorldKey::Interface(_) => func.name.clone(),
                };
                fields.push(build_field(None, ExportLocation::Root, &func_kebab, func));
            }
            WorldItem::Interface { id, .. } => {
                // Prefer the export-key name (`export incidents-v2;` → `WorldKey::Name`), falling
                // back to the interface's own name — matching `schema::collect_world`.
                let iface_kebab = match key {
                    WorldKey::Name(n) => Some(n.clone()),
                    WorldKey::Interface(_) => resolve.interfaces[*id].name.clone(),
                };
                let export = match interface_export_name(resolve, *id) {
                    Some(instance_name) => ExportLocation::Interface { instance_name },
                    // A named export with no resolvable package name is unexpected; treat the
                    // function as root-exported rather than dropping it silently.
                    None => ExportLocation::Root,
                };
                for (func_kebab, func) in resolve.interfaces[*id].functions.iter() {
                    fields.push(build_field(
                        iface_kebab.as_deref(),
                        export.clone(),
                        func_kebab,
                        func,
                    ));
                }
            }
            WorldItem::Type { .. } => {}
        }
    }

    OperationMap { fields }
}

fn build_field(
    iface_kebab: Option<&str>,
    export: ExportLocation,
    func_kebab: &str,
    func: &Function,
) -> FieldMapping {
    // GraphQL field name qualifies the function by its owning interface — identical to
    // `schema::collect_function`. Classification uses the BARE function name.
    let qualified = match iface_kebab {
        Some(i) => format!("{}-{}", i, func_kebab),
        None => func_kebab.to_string(),
    };
    let graphql_field = to_camel_case(&qualified);
    let kind = match classify(func_kebab) {
        FieldKind::Query => OpKind::Query,
        FieldKind::Mutation => OpKind::Mutation,
    };
    let params = func
        .params
        .iter()
        .map(|p| ParamMapping {
            graphql_arg: to_camel_case(&p.name),
            wit_param: p.name.clone(),
        })
        .collect();

    FieldMapping {
        graphql_field,
        kind,
        export,
        func: func_kebab.to_string(),
        params,
    }
}

/// Build the fully-qualified component-model export name for an interface,
/// e.g. `incidentio:api/incidents-v2@0.1.0`. Returns `None` if the interface is
/// unnamed or has no resolvable package.
fn interface_export_name(resolve: &Resolve, iface_id: InterfaceId) -> Option<String> {
    let iface = &resolve.interfaces[iface_id];
    let iface_name = iface.name.as_ref()?;
    let pkg = &resolve.packages[iface.package?];
    let name = &pkg.name;
    let mut export = format!("{}:{}/{}", name.namespace, name.name, iface_name);
    if let Some(version) = &name.version {
        export.push('@');
        export.push_str(&version.to_string());
    }
    Some(export)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wit_parser::{Function, FunctionKind, Param, Stability, Type, World};

    #[test]
    fn root_function_mapping() {
        let mut resolve = Resolve::default();
        let world_id = resolve.worlds.alloc(World {
            name: "test".into(),
            imports: Default::default(),
            exports: Default::default(),
            package: None,
            docs: Default::default(),
            stability: Stability::Unknown,
            includes: vec![],
            span: Default::default(),
        });
        let func = Function {
            name: "get-user".into(),
            kind: FunctionKind::Freestanding,
            params: vec![Param {
                name: "user-name".into(),
                ty: Type::String,
                span: Default::default(),
            }],
            result: None,
            docs: Default::default(),
            stability: Stability::Unknown,
            span: Default::default(),
        };
        resolve.worlds[world_id]
            .exports
            .insert(WorldKey::Name("get-user".into()), WorldItem::Function(func));

        let map = operation_map(&resolve, world_id);
        let f = map.get("getUser").expect("field getUser");
        assert_eq!(f.kind, OpKind::Query);
        assert_eq!(f.export, ExportLocation::Root);
        assert_eq!(f.func, "get-user");
        assert_eq!(
            f.params,
            vec![ParamMapping {
                graphql_arg: "userName".into(),
                wit_param: "user-name".into(),
            }]
        );
    }

    // Interface-level mapping (interface-qualified field names + fully-qualified export instance
    // names like `incidentio:api/incidents-v2@0.1.0`) is validated against the real
    // incident-io-component in the wasmtime spike, rather than hand-building wit_parser package /
    // interface structs here (their shapes are version-sensitive).
}
