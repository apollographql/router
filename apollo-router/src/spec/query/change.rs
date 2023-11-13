use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::Hash;
use std::hash::Hasher;

use apollo_compiler::ast;
use apollo_compiler::ast::Selection;
use apollo_compiler::schema;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Node;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;

use super::transform;
use super::traverse;

pub(crate) struct QueryHashVisitor<'a> {
    schema: &'a schema::Schema,
    hasher: Sha256,
    fragments: HashMap<&'a ast::Name, &'a ast::FragmentDefinition>,
    hashed_types: HashSet<String>,
    // name, field
    hashed_fields: HashSet<(String, String)>,
    pub(crate) subgraph_query: bool,
}

#[allow(dead_code)]
impl<'a> QueryHashVisitor<'a> {
    pub(crate) fn new(schema: &'a schema::Schema, executable: &'a ast::Document) -> Option<Self> {
        Some(Self {
            schema,
            hasher: Sha256::new(),
            fragments: transform::collect_fragments(executable),
            hashed_types: HashSet::new(),
            hashed_fields: HashSet::new(),
            subgraph_query: false,
        })
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.hasher.finalize().as_slice().into()
    }

    fn hash_directive(&mut self, directive: &Node<ast::Directive>) {
        directive.name.as_str().hash(self);
        for argument in &directive.arguments {
            self.hash_argument(argument)
        }
    }

    fn hash_argument(&mut self, argument: &Node<ast::Argument>) {
        argument.name.hash(self);
        argument.value.hash(self);
    }

    fn hash_type_by_name(&mut self, t: &str) {
        if self.hashed_types.contains(t) {
            return;
        }

        self.hashed_types.insert(t.to_string());

        if let Some(ty) = self.schema.types.get(t) {
            self.hash_extended_type(ty);
        }
    }

    fn hash_extended_type(&mut self, t: &'a ExtendedType) {
        match t {
            ExtendedType::Scalar(s) => {
                for directive in &s.directives {
                    self.hash_directive(&directive.node);
                }
            }
            ExtendedType::Object(o) => {
                for directive in &o.directives {
                    self.hash_directive(&directive.node);
                }
            }
            ExtendedType::Interface(i) => {
                for directive in &i.directives {
                    self.hash_directive(&directive.node);
                }
            }
            ExtendedType::Union(u) => {
                for directive in &u.directives {
                    self.hash_directive(&directive.node);
                }

                for member in &u.members {
                    self.hash_type_by_name(member.as_str());
                }
            }
            ExtendedType::Enum(e) => {
                for directive in &e.directives {
                    self.hash_directive(&directive.node);
                }

                for (value, def) in &e.values {
                    value.hash(self);
                    for directive in &def.directives {
                        self.hash_directive(directive);
                    }
                }
            }
            ExtendedType::InputObject(o) => {
                for directive in &o.directives {
                    self.hash_directive(&directive.node);
                }

                for (name, ty) in &o.fields {
                    if ty.default_value.is_some() {
                        name.hash(self);
                        self.hash_input_value_definition(&ty.node);
                    }
                }
            }
        }
    }

    fn hash_type(&mut self, t: &ast::Type) {
        match t {
            schema::Type::Named(name) => self.hash_type_by_name(name.as_str()),
            schema::Type::NonNullNamed(name) => {
                "!".hash(self);
                self.hash_type_by_name(name.as_str())
            }
            schema::Type::List(t) => {
                "[]".hash(self);
                self.hash_type(t);
            }
            schema::Type::NonNullList(t) => {
                "[]!".hash(self);
                self.hash_type(t);
            }
        }
    }

    fn hash_input_value_definition(&mut self, t: &Node<ast::InputValueDefinition>) {
        self.hash_type(&t.ty);
        for directive in &t.directives {
            self.hash_directive(directive);
        }
        t.default_value.hash(self);
    }
}

impl<'a> Hasher for QueryHashVisitor<'a> {
    fn finish(&self) -> u64 {
        //self.hasher.finalize()
        todo!()
    }

    fn write(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }
}

impl<'a> traverse::Visitor for QueryHashVisitor<'a> {
    fn operation(
        &mut self,
        root_type: &str,
        node: &ast::OperationDefinition,
    ) -> Result<(), BoxError> {
        println!("looking at root type {root_type} and operation {:#?}", node);
        root_type.hash(self);
        self.hash_type_by_name(root_type);

        if !self.subgraph_query {
            traverse::operation(self, root_type, node)
        } else {
            if node.selection_set.len() != 1 {
                return Err("invalid number of selections for _entities query".into());
            }

            match node.selection_set.first() {
                Some(Selection::Field(field)) => {
                    if field.name.as_str() != "_entities" {
                        return Err("expected _entities field".into());
                    }

                    "_entities".hash(self);

                    for selection in &field.selection_set {
                        match selection {
                            Selection::InlineFragment(f) => {
                                match f.type_condition.as_ref() {
                                    None => {
                                        return Err("expected type condition".into());
                                    }
                                    Some(condition) => {
                                        self.inline_fragment(condition.as_str(), f)?
                                    }
                                };
                            }
                            _ => return Err("expected inline fragment".into()),
                        }
                    }
                    Ok(())
                }
                _ => Err("expected _entities field".into()),
            }
            //todo!()
            /*for selection in node.selection_set {
                match
            }*/
        }
    }

    fn field(
        &mut self,
        parent_type: &str,
        field_def: &ast::FieldDefinition,
        node: &ast::Field,
    ) -> Result<(), BoxError> {
        let parent = parent_type.to_string();
        let name = field_def.name.as_str().to_string();
        if self.hashed_fields.insert((parent, name)) {
            self.hash_type_by_name(parent_type);

            field_def.name.hash(self);

            for argument in &field_def.arguments {
                self.hash_input_value_definition(argument);
            }

            self.hash_type(&field_def.ty);

            for directive in &field_def.directives {
                self.hash_directive(directive);
            }
        }

        traverse::field(self, field_def, node)
    }

    fn fragment_definition(&mut self, node: &ast::FragmentDefinition) -> Result<(), BoxError> {
        self.hash_type_by_name(&node.type_condition);

        traverse::fragment_definition(self, node)
    }

    fn fragment_spread(&mut self, node: &ast::FragmentSpread) -> Result<(), BoxError> {
        let type_condition = &self
            .fragments
            .get(&node.fragment_name)
            .ok_or("MissingFragment")?
            .type_condition;
        self.hash_type_by_name(type_condition);

        traverse::fragment_spread(self, node)
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        node: &ast::InlineFragment,
    ) -> Result<(), BoxError> {
        if let Some(type_condition) = &node.type_condition {
            self.hash_type_by_name(type_condition);
        }
        traverse::inline_fragment(self, parent_type, node)
    }

    fn schema(&self) -> &apollo_compiler::Schema {
        self.schema
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ast::Document;
    use apollo_compiler::schema::Schema;

    use super::QueryHashVisitor;
    use crate::spec::query::traverse;

    #[track_caller]
    fn hash(schema: &str, query: &str) -> String {
        let schema = Schema::parse(schema, "schema.graphql");
        let doc = Document::parse(query, "query.graphql");
        schema.validate().unwrap();
        doc.to_executable(&schema).validate(&schema).unwrap();
        let mut visitor = QueryHashVisitor::new(&schema, &doc).unwrap();
        traverse::document(&mut visitor, &doc).unwrap();

        hex::encode(visitor.finish())
    }

    #[test]
    fn me() {
        let schema1: &str = r#"
        schema {
          query: Query
        }
    
        type Query {
          me: User
          customer: User
        }
    
        type User {
          id: ID
          name: String
        }
        "#;

        let schema2: &str = r#"
        schema {
            query: Query
        }
    
        type Query {
          me: User
        }
    
    
        type User {
          id: ID!
          name: String
        }
        "#;
        let query = "query { me { name } }";
        assert_eq!(hash(schema1, query), hash(schema2, query));

        // id is nullable in 1, non nullable in 2
        let query = "query { me { id name } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        // simple normalization
        let query = "query {  moi: me { name   } }";
        assert_eq!(hash(schema1, query), hash(schema2, query));

        assert_ne!(
            hash(schema1, "query { me { id name } }"),
            hash(schema1, "query { me { name id } }")
        );
    }

    #[test]
    fn directive() {
        let schema1: &str = r#"
        schema {
          query: Query
        }
        directive @test on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    
        type Query {
          me: User
          customer: User
        }
    
        type User {
          id: ID!
          name: String
        }
        "#;

        let schema2: &str = r#"
        schema {
            query: Query
        }
        directive @test on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    
        type Query {
          me: User
          customer: User @test
        }
    
    
        type User {
          id: ID! @test
          name: String
        }
        "#;
        let query = "query { me { name } }";
        assert_eq!(hash(schema1, query), hash(schema2, query));

        let query = "query { me { id name } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        let query = "query { customer { id } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));
    }

    #[test]
    fn interface() {
        let schema1: &str = r#"
        schema {
          query: Query
        }
        directive @test on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    
        type Query {
          me: User
          customer: I
        }

        interface I {
            id: ID
        }
    
        type User implements I {
          id: ID!
          name: String
        }
        "#;

        let schema2: &str = r#"
        schema {
            query: Query
        }
        directive @test on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    
        type Query {
          me: User
          customer: I
        }

        interface I @test {
            id: ID
        }
    
        type User implements I {
          id: ID!
          name: String
        }
        "#;

        let query = "query { me { id name } }";
        assert_eq!(hash(schema1, query), hash(schema2, query));

        let query = "query { customer { id } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        let query = "query { customer { ... on User { name } } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));
    }

    #[test]
    fn arguments() {
        let schema1: &str = r#"
        type Query {
          a(i: Int): Int
          b(i: Int = 1): Int
          c(i: Int = 1, j: Int): Int
        }
        "#;

        let schema2: &str = r#"
        type Query {
            a(i: Int!): Int
            b(i: Int = 2): Int
            c(i: Int = 2, j: Int): Int
          }
        "#;

        let query = "query { a(i: 0) }";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        let query = "query { b }";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        let query = "query { b(i: 0)}";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        let query = "query { c(j: 0)}";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        let query = "query { c(i:0, j: 0)}";
        assert_ne!(hash(schema1, query), hash(schema2, query));
    }

    #[test]
    fn entities() {
        let schema1: &str = r#"
        schema {
          query: Query
        }
    
        type Query {
          me: User
          customer: User
        }
    
        type User {
          id: ID
          name: String
        }
        "#;

        let schema2: &str = r#"
        schema {
            query: Query
        }
    
        type Query {
          me: User
        }
    
    
        type User {
          id: ID!
          name: String
        }
        "#;
        let query = r#"Query1($representations:[_Any!]!){
            _entities(representations:$representations){
                ...on User {
                    id
                    name
                }
            }
        }"#;
        //assert_eq!(hash(schema1, query), hash(schema2, query));

        let schema = Schema::parse(schema1, "schema.graphql");
        let doc = Document::parse(query, "query.graphql");
        //schema.validate().unwrap();
        //doc.to_executable(&schema).validate(&schema).unwrap();
        let mut visitor = QueryHashVisitor::new(&schema, &doc).unwrap();
        visitor.subgraph_query = true;
        traverse::document(&mut visitor, &doc).unwrap();

        let hash1 = hex::encode(visitor.finish());
        println!("hash1: {hash1}");
        let doc = Document::parse(query, "query.graphql");
        //schema.validate().unwrap();
        //doc.to_executable(&schema).validate(&schema).unwrap();
        let mut visitor = QueryHashVisitor::new(&schema, &doc).unwrap();
        visitor.subgraph_query = true;
        traverse::document(&mut visitor, &doc).unwrap();

        let hash2 = hex::encode(visitor.finish());
        println!("hash2: {hash2}");
        assert_eq!(hash1, hash2);
    }
}
