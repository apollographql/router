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
use crate::plugins::cache::entity::ENTITIES;

/// Calculates a hash of the query and the schema, but only looking at the parts of the
/// schema which affect the query.
/// This means that if a schema update does not affect an existing query (example: adding a field),
/// then the hash will stay the same
pub(crate) struct QueryHashVisitor<'a> {
    schema: &'a schema::Schema,
    hasher: Sha256,
    fragments: HashMap<&'a ast::Name, &'a ast::FragmentDefinition>,
    hashed_types: HashSet<String>,
    // name, field
    hashed_fields: HashSet<(String, String)>,
    pub(crate) subgraph_query: bool,
}

impl<'a> QueryHashVisitor<'a> {
    pub(crate) fn new(schema: &'a schema::Schema, executable: &'a ast::Document) -> Self {
        Self {
            schema,
            hasher: Sha256::new(),
            fragments: transform::collect_fragments(executable),
            hashed_types: HashSet::new(),
            hashed_fields: HashSet::new(),
            subgraph_query: false,
        }
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
        self.hash_value(&argument.value);
    }

    fn hash_value(&mut self, value: &ast::Value) {
        match value {
            schema::Value::Null => "null".hash(self),
            schema::Value::Enum(e) => {
                "enum".hash(self);
                e.hash(self);
            }
            schema::Value::Variable(v) => {
                "variable".hash(self);
                v.hash(self);
            }
            schema::Value::String(s) => {
                "string".hash(self);
                s.hash(self);
            }
            schema::Value::Float(f) => {
                "float".hash(self);
                f.hash(self);
            }
            schema::Value::Int(i) => {
                "int".hash(self);
                i.hash(self);
            }
            schema::Value::Boolean(b) => {
                "boolean".hash(self);
                b.hash(self);
            }
            schema::Value::List(l) => {
                "list[".hash(self);
                for v in l.iter() {
                    self.hash_value(v);
                }
                "]".hash(self);
            }
            schema::Value::Object(o) => {
                "object{".hash(self);
                for (k, v) in o.iter() {
                    k.hash(self);
                    ":".hash(self);
                    self.hash_value(v);
                }
                "}".hash(self);
            }
        }
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
        if let Some(value) = t.default_value.as_ref() {
            self.hash_value(value);
        }
    }

    fn hash_entities_operation(&mut self, node: &ast::OperationDefinition) -> Result<(), BoxError> {
        use crate::spec::query::traverse::Visitor;

        if node.selection_set.len() != 1 {
            return Err("invalid number of selections for _entities query".into());
        }

        match node.selection_set.first() {
            Some(Selection::Field(field)) => {
                if field.name.as_str() != ENTITIES {
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
                                Some(condition) => self.inline_fragment(condition.as_str(), f)?,
                            };
                        }
                        Selection::FragmentSpread(f) => self.fragment_spread(f)?,
                        _ => return Err("expected inline fragment".into()),
                    }
                }
                Ok(())
            }
            _ => Err("expected _entities field".into()),
        }
    }
}

impl<'a> Hasher for QueryHashVisitor<'a> {
    fn finish(&self) -> u64 {
        unreachable!()
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
        root_type.hash(self);
        self.hash_type_by_name(root_type);

        if !self.subgraph_query {
            traverse::operation(self, root_type, node)
        } else {
            self.hash_entities_operation(node)
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

            for argument in &node.arguments {
                self.hash_argument(argument);
            }

            self.hash_type(&field_def.ty);

            for directive in &field_def.directives {
                self.hash_directive(directive);
            }
        }

        traverse::field(self, field_def, node)
    }

    fn fragment_definition(&mut self, node: &ast::FragmentDefinition) -> Result<(), BoxError> {
        node.name.hash(self);
        self.hash_type_by_name(&node.type_condition);

        traverse::fragment_definition(self, node)
    }

    fn fragment_spread(&mut self, node: &ast::FragmentSpread) -> Result<(), BoxError> {
        node.fragment_name.hash(self);
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
        let schema = Schema::parse(schema, "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let doc = Document::parse(query, "query.graphql").unwrap();

        doc.to_executable(&schema)
            .unwrap()
            .validate(&schema)
            .unwrap();
        let mut visitor = QueryHashVisitor::new(&schema, &doc);
        traverse::document(&mut visitor, &doc).unwrap();

        hex::encode(visitor.finish())
    }

    #[track_caller]
    fn hash_subgraph_query(schema: &str, query: &str) -> String {
        let schema = Schema::parse(schema, "schema.graphql").unwrap();
        let doc = Document::parse(query, "query.graphql").unwrap();
        //doc.to_executable(&schema);
        let mut visitor = QueryHashVisitor::new(&schema, &doc);
        visitor.subgraph_query = true;
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

        let query1 = r#"query Query1($representations:[_Any!]!){
            _entities(representations:$representations){
                ...on User {
                    id
                    name
                }
            }
        }"#;

        println!("query1: {query1}");

        let hash1 = hash_subgraph_query(schema1, query1);
        println!("hash1: {hash1}");

        let hash2 = hash_subgraph_query(schema2, query1);
        println!("hash2: {hash2}");

        assert_ne!(hash1, hash2);

        let query2 = r#"query Query1($representations:[_Any!]!){
            _entities(representations:$representations){
                ...on User {
                    name
                }
            }
        }"#;

        println!("query2: {query2}");

        let hash1 = hash_subgraph_query(schema1, query2);
        println!("hash1: {hash1}");

        let hash2 = hash_subgraph_query(schema2, query2);
        println!("hash2: {hash2}");

        assert_eq!(hash1, hash2);
    }
}
