use crate::*;
use apollo_parser::ast;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct Schema {
    string: String,
    subtype_map: HashMap<String, HashSet<String>>,
    subgraphs: HashMap<String, String>,
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
        let mut subgraphs = HashMap::new();

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
                ast::Definition::EnumTypeDefinition(enum_type) => {
                    if enum_type
                        .name()
                        .and_then(|n| n.ident_token())
                        .as_ref()
                        .map(|id| id.text())
                        == Some("join__Graph")
                    {
                        if let Some(enums) = enum_type.enum_values_definition() {
                            for enum_kind in enums.enum_value_definitions() {
                                if let Some(directives) = enum_kind.directives() {
                                    for directive in directives.directives() {
                                        if directive
                                            .name()
                                            .and_then(|n| n.ident_token())
                                            .as_ref()
                                            .map(|id| id.text())
                                            == Some("join__graph")
                                        {
                                            let mut name = None;
                                            let mut url = None;

                                            if let Some(arguments) = directive.arguments() {
                                                for argument in arguments.arguments() {
                                                    let arg_name = argument
                                                        .name()
                                                        .and_then(|n| n.ident_token())
                                                        .as_ref()
                                                        .map(|id| id.text().to_owned());

                                                    let arg_value = argument.value().map(|s| {
                                                        let mut without_quotes = s.to_string();
                                                        without_quotes.retain(|c| c != '"');
                                                        without_quotes.trim().to_string()
                                                    });

                                                    match arg_name.as_deref() {
                                                        Some("name") => name = arg_value,
                                                        Some("url") => url = arg_value,
                                                        _ => {}
                                                    };
                                                }
                                            }
                                            if let (Some(name), Some(url)) = (name, url) {
                                                // FIXME: return an error on name collisions
                                                subgraphs.insert(name, url);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(Self {
            subtype_map,
            string: s.to_owned(),
            subgraphs,
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

    pub fn subgraphs(&self) -> impl Iterator<Item = (&String, &String)> {
        self.subgraphs.iter()
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

    #[test]
    fn routing_urls() {
        let schema: Schema = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1"),
        @core(feature: "https://specs.apollo.dev/join/v0.1")
      {
        query: Query
        mutation: Mutation
      }

      enum join__Graph {
        ACCOUNTS @join__graph(name:"accounts" url: "http://localhost:4001/graphql")
        INVENTORY
          @join__graph(name: "inventory", url: "http://localhost:4004/graphql")
        PRODUCTS
        @join__graph(name: "products" url: "http://localhost:4003/graphql")
        REVIEWS @join__graph(name: "reviews" url: "http://localhost:4002/graphql")
      }"#
        .parse()
        .unwrap();

        println!("subgraphs: {:?}", schema.subgraphs);
        assert_eq!(schema.subgraphs.len(), 4);
        assert_eq!(
            schema.subgraphs.get("accounts").map(|s| s.as_str()),
            Some("http://localhost:4001/graphql")
        );

        assert_eq!(schema.subgraphs.get("test"), None);
    }
}
