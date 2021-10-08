use crate::*;
use apollo_parser::ast;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct Schema {
    string: String,
    subtype_map: HashMap<String, HashSet<String>>,
}

impl std::str::FromStr for Schema {
    type Err = SchemaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parser = apollo_parser::Parser::new(s);
        let tree = parser.parse();

        if !tree.errors().is_empty() {
            return Err(SchemaError::ParseErrors(tree.errors().to_vec()));
        }

        let document = tree.document();
        let mut subtype_map: HashMap<String, HashSet<String>> = Default::default();

        // the logic of this algorithm is inspired from the npm package graphql:
        // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L302-L327
        // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L294-L300
        // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L215-L263
        for definition in document.definitions() {
            macro_rules! implements_interfaces {
                ($definition:expr) => {{
                    let name = $definition
                        .name()
                        .expect("never optional according to spec; qed")
                        .text()
                        .to_string();

                    for key in $definition
                        .implements_interfaces()
                        .iter()
                        .flat_map(|member_types| member_types.named_types().flat_map(|x| x.name()))
                    {
                        let key = key.text().to_string();
                        let set = subtype_map.entry(key).or_default();
                        set.insert(name.clone());
                    }
                }};
            }

            macro_rules! union_member_types {
                ($definition:expr) => {{
                    let key = $definition
                        .name()
                        .expect("never optional according to spec; qed")
                        .text()
                        .to_string();
                    let set = subtype_map.entry(key).or_default();

                    for name in $definition
                        .union_member_types()
                        .iter()
                        .flat_map(|member_types| member_types.named_types().flat_map(|x| x.name()))
                    {
                        set.insert(name.text().to_string());
                    }
                }};
            }

            match definition {
                // Spec: https://spec.graphql.org/draft/#ObjectTypeDefinition
                ast::Definition::ObjectTypeDefinition(object) => implements_interfaces!(object),
                // Spec: https://spec.graphql.org/draft/#InterfaceTypeDefinition
                ast::Definition::InterfaceTypeDefinition(interface) => {
                    implements_interfaces!(interface)
                }
                // Spec: https://spec.graphql.org/draft/#UnionTypeDefinition
                ast::Definition::UnionTypeDefinition(union) => union_member_types!(union),
                // Spec: https://spec.graphql.org/draft/#sec-Object-Extensions
                ast::Definition::ObjectTypeExtension(object) => implements_interfaces!(object),
                // Spec: https://spec.graphql.org/draft/#sec-Interface-Extensions
                ast::Definition::InterfaceTypeExtension(interface) => {
                    implements_interfaces!(interface)
                }
                // Spec: https://spec.graphql.org/draft/#sec-Union-Extensions
                ast::Definition::UnionTypeExtension(union) => union_member_types!(union),
                _ => {}
            }
        }

        Ok(Self {
            subtype_map,
            string: s.to_owned(),
        })
    }
}

impl Schema {
    pub fn read(path: impl AsRef<std::path::Path>) -> Result<Self, SchemaError> {
        std::fs::read_to_string(path)?.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.string
    }

    pub fn is_subtype(&self, abstract_type: &str, maybe_subtype: &str) -> bool {
        self.subtype_map
            .get(abstract_type)
            .map(|x| x.contains(maybe_subtype))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_subtype() {
        let schema: Schema = "union UnionType = Foo | Bar | Baz".parse().unwrap();
        assert!(schema.is_subtype("UnionType", "Foo"));
        assert!(schema.is_subtype("UnionType", "Bar"));
        assert!(schema.is_subtype("UnionType", "Baz"));
        let schema: Schema = "type ObjectType implements Foo & Bar & Baz { }"
            .parse()
            .unwrap();
        assert!(schema.is_subtype("Foo", "ObjectType"));
        assert!(schema.is_subtype("Bar", "ObjectType"));
        assert!(schema.is_subtype("Baz", "ObjectType"));
        let schema: Schema = "interface InterfaceType implements Foo & Bar & Baz { }"
            .parse()
            .unwrap();
        assert!(schema.is_subtype("Foo", "InterfaceType"));
        assert!(schema.is_subtype("Bar", "InterfaceType"));
        assert!(schema.is_subtype("Baz", "InterfaceType"));
        let schema: Schema = "extend union UnionType = Foo | Bar | Baz".parse().unwrap();
        assert!(schema.is_subtype("UnionType", "Foo"));
        assert!(schema.is_subtype("UnionType", "Bar"));
        assert!(schema.is_subtype("UnionType", "Baz"));
        let schema: Schema = "extend type ObjectType implements Foo & Bar & Baz { }"
            .parse()
            .unwrap();
        assert!(schema.is_subtype("Foo", "ObjectType"));
        assert!(schema.is_subtype("Bar", "ObjectType"));
        assert!(schema.is_subtype("Baz", "ObjectType"));
        let schema: Schema = "extend interface InterfaceType implements Foo & Bar & Baz { }"
            .parse()
            .unwrap();
        assert!(schema.is_subtype("Foo", "InterfaceType"));
        assert!(schema.is_subtype("Bar", "InterfaceType"));
        assert!(schema.is_subtype("Baz", "InterfaceType"));
    }
}
