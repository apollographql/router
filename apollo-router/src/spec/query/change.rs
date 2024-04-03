use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::Hash;
use std::hash::Hasher;

use apollo_compiler::ast;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Name;
use apollo_compiler::executable;
use apollo_compiler::schema;
use apollo_compiler::schema::DirectiveList;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;
use apollo_compiler::Parser;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;

use super::traverse;
use super::traverse::Visitor;
use crate::plugins::progressive_override::JOIN_FIELD_DIRECTIVE_NAME;
use crate::plugins::progressive_override::JOIN_SPEC_BASE_URL;
use crate::spec::Schema;

pub(crate) const JOIN_TYPE_DIRECTIVE_NAME: &str = "join__type";

/// Calculates a hash of the query and the schema, but only looking at the parts of the
/// schema which affect the query.
/// This means that if a schema update does not affect an existing query (example: adding a field),
/// then the hash will stay the same
pub(crate) struct QueryHashVisitor<'a> {
    schema: &'a schema::Schema,
    hasher: Sha256,
    fragments: HashMap<&'a ast::Name, &'a Node<executable::Fragment>>,
    hashed_types: HashSet<String>,
    // name, field
    hashed_fields: HashSet<(String, String)>,
    join_field_directive_name: Option<String>,
    join_type_directive_name: Option<String>,
}

impl<'a> QueryHashVisitor<'a> {
    pub(crate) fn new(
        schema: &'a schema::Schema,
        executable: &'a executable::ExecutableDocument,
    ) -> Self {
        Self {
            schema,
            hasher: Sha256::new(),
            fragments: executable.fragments.iter().collect(),
            hashed_types: HashSet::new(),
            hashed_fields: HashSet::new(),
            // should we just return an error if we do not find those directives?
            join_field_directive_name: Schema::directive_name(
                schema,
                JOIN_SPEC_BASE_URL,
                ">=0.1.0",
                JOIN_FIELD_DIRECTIVE_NAME,
            ),
            join_type_directive_name: Schema::directive_name(
                schema,
                JOIN_SPEC_BASE_URL,
                ">=0.1.0",
                JOIN_TYPE_DIRECTIVE_NAME,
            ),
        }
    }

    pub(crate) fn hash_query(
        schema: &'a schema::Schema,
        executable: &'a executable::ExecutableDocument,
        operation_name: Option<&str>,
    ) -> Result<Vec<u8>, BoxError> {
        let mut visitor = QueryHashVisitor::new(schema, executable);
        traverse::document(&mut visitor, executable, operation_name)?;
        Ok(visitor.finish())
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

    fn hash_type_by_name(&mut self, t: &str) -> Result<(), BoxError> {
        if self.hashed_types.contains(t) {
            return Ok(());
        }

        self.hashed_types.insert(t.to_string());

        if let Some(ty) = self.schema.types.get(t) {
            self.hash_extended_type(ty)?;
        }
        Ok(())
    }

    fn hash_extended_type(&mut self, t: &'a ExtendedType) -> Result<(), BoxError> {
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

                self.hash_join_type(&o.name, &o.directives)?;
            }
            ExtendedType::Interface(i) => {
                for directive in &i.directives {
                    self.hash_directive(&directive.node);
                }
                self.hash_join_type(&i.name, &i.directives)?;
            }
            ExtendedType::Union(u) => {
                for directive in &u.directives {
                    self.hash_directive(&directive.node);
                }

                for member in &u.members {
                    self.hash_type_by_name(member.as_str())?;
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
                        self.hash_input_value_definition(&ty.node)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn hash_type(&mut self, t: &ast::Type) -> Result<(), BoxError> {
        match t {
            schema::Type::Named(name) => self.hash_type_by_name(name.as_str()),
            schema::Type::NonNullNamed(name) => {
                "!".hash(self);
                self.hash_type_by_name(name.as_str())
            }
            schema::Type::List(t) => {
                "[]".hash(self);
                self.hash_type(t)
            }
            schema::Type::NonNullList(t) => {
                "[]!".hash(self);
                self.hash_type(t)
            }
        }
    }

    fn hash_field(
        &mut self,
        parent_type: String,
        type_name: String,
        field_def: &FieldDefinition,
        arguments: &[Node<Argument>],
    ) -> Result<(), BoxError> {
        if self.hashed_fields.insert((parent_type.clone(), type_name)) {
            self.hash_type_by_name(&parent_type)?;

            field_def.name.hash(self);

            for argument in &field_def.arguments {
                self.hash_input_value_definition(argument)?;
            }

            for argument in arguments {
                self.hash_argument(argument);
            }

            self.hash_type(&field_def.ty)?;

            for directive in &field_def.directives {
                self.hash_directive(directive);
            }

            self.hash_join_field(&parent_type, &field_def.directives)?;
        }
        Ok(())
    }

    fn hash_input_value_definition(
        &mut self,
        t: &Node<ast::InputValueDefinition>,
    ) -> Result<(), BoxError> {
        self.hash_type(&t.ty)?;
        for directive in &t.directives {
            self.hash_directive(directive);
        }
        if let Some(value) = t.default_value.as_ref() {
            self.hash_value(value);
        }
        Ok(())
    }

    fn hash_join_type(&mut self, name: &Name, directives: &DirectiveList) -> Result<(), BoxError> {
        if let Some(dir_name) = self.join_type_directive_name.as_deref() {
            if let Some(dir) = directives.get(dir_name) {
                if let Some(key) = dir.argument_by_name("key").and_then(|arg| arg.as_str()) {
                    let mut parser = Parser::new();
                    if let Ok(field_set) = parser.parse_field_set(
                        Valid::assume_valid_ref(self.schema),
                        name.clone(),
                        key,
                        std::path::Path::new("schema.graphql"),
                    ) {
                        traverse::selection_set(
                            self,
                            name.as_str(),
                            &field_set.selection_set.selections[..],
                        )?;
                    }
                }
            }
        }

        Ok(())
    }

    fn hash_join_field(
        &mut self,
        parent_type: &str,
        directives: &ast::DirectiveList,
    ) -> Result<(), BoxError> {
        if let Some(dir_name) = self.join_field_directive_name.as_deref() {
            if let Some(dir) = directives.get(dir_name) {
                if let Some(requires) = dir
                    .argument_by_name("requires")
                    .and_then(|arg| arg.as_str())
                {
                    if let Ok(parent_type) = Name::new(NodeStr::new(parent_type)) {
                        let mut parser = Parser::new();

                        if let Ok(field_set) = parser.parse_field_set(
                            Valid::assume_valid_ref(self.schema),
                            parent_type.clone(),
                            requires,
                            std::path::Path::new("schema.graphql"),
                        ) {
                            traverse::selection_set(
                                self,
                                parent_type.as_str(),
                                &field_set.selection_set.selections[..],
                            )?;
                        }
                    }
                }
            }
        }

        Ok(())
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

impl<'a> Visitor for QueryHashVisitor<'a> {
    fn operation(&mut self, root_type: &str, node: &executable::Operation) -> Result<(), BoxError> {
        root_type.hash(self);
        self.hash_type_by_name(root_type)?;

        traverse::operation(self, root_type, node)
    }

    fn field(
        &mut self,
        parent_type: &str,
        field_def: &ast::FieldDefinition,
        node: &executable::Field,
    ) -> Result<(), BoxError> {
        self.hash_field(
            parent_type.to_string(),
            field_def.name.as_str().to_string(),
            field_def,
            &node.arguments,
        )?;

        traverse::field(self, field_def, node)
    }

    fn fragment(&mut self, node: &executable::Fragment) -> Result<(), BoxError> {
        node.name.hash(self);
        self.hash_type_by_name(node.type_condition())?;

        traverse::fragment(self, node)
    }

    fn fragment_spread(&mut self, node: &executable::FragmentSpread) -> Result<(), BoxError> {
        node.fragment_name.hash(self);
        let type_condition = &self
            .fragments
            .get(&node.fragment_name)
            .ok_or("MissingFragment")?
            .type_condition();
        self.hash_type_by_name(type_condition)?;

        traverse::fragment_spread(self, node)
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        node: &executable::InlineFragment,
    ) -> Result<(), BoxError> {
        if let Some(type_condition) = &node.type_condition {
            self.hash_type_by_name(type_condition)?;
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
    use apollo_compiler::validation::Valid;

    use super::QueryHashVisitor;
    use crate::spec::query::traverse;

    #[track_caller]
    fn hash(schema: &str, query: &str) -> String {
        let schema = Schema::parse(schema, "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let doc = Document::parse(query, "query.graphql").unwrap();

        let exec = doc
            .to_executable(&schema)
            .unwrap()
            .validate(&schema)
            .unwrap();
        let mut visitor = QueryHashVisitor::new(&schema, &exec);
        traverse::document(&mut visitor, &exec, None).unwrap();

        hex::encode(visitor.finish())
    }

    #[track_caller]
    fn hash_subgraph_query(schema: &str, query: &str) -> String {
        let schema = Valid::assume_valid(Schema::parse(schema, "schema.graphql").unwrap());
        let doc = Document::parse(query, "query.graphql").unwrap();
        let exec = doc
            .to_executable(&schema)
            .unwrap()
            .validate(&schema)
            .unwrap();
        let mut visitor = QueryHashVisitor::new(&schema, &exec);
        traverse::document(&mut visitor, &exec, None).unwrap();

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
    
        scalar _Any

        union _Entity = User

        type Query {
        _entities(representations: [_Any!]!): [_Entity]!
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
    
        scalar _Any

        union _Entity = User

        type Query {
          _entities(representations: [_Any!]!): [_Entity]!
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

    #[test]
    fn join_type_key() {
        let schema1: &str = r#"
        schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
          query: Query
        }
        directive @test on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
        directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        scalar join__FieldSet

        scalar link__Import

        enum link__Purpose {
            """
            `SECURITY` features provide metadata necessary to securely resolve fields.
            """
            SECURITY

            """
            `EXECUTION` features provide metadata necessary for operation execution.
            """
            EXECUTION
        }

        enum join__Graph {
            ACCOUNTS @join__graph(name: "accounts", url: "https://accounts.demo.starstuff.dev")
            INVENTORY @join__graph(name: "inventory", url: "https://inventory.demo.starstuff.dev")
            PRODUCTS @join__graph(name: "products", url: "https://products.demo.starstuff.dev")
            REVIEWS @join__graph(name: "reviews", url: "https://reviews.demo.starstuff.dev")
        }

        type Query {
          me: User
          customer: User
          itf: I
        }

        type User @join__type(graph: ACCOUNTS, key: "id") {
          id: ID!
          name: String
        }

        interface I @join__type(graph: ACCOUNTS, key: "id") {
            id: ID!
            name :String
        }

        union U = User
        "#;

        let schema2: &str = r#"
        schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
            query: Query
        }
        directive @test on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
        directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        scalar join__FieldSet

        scalar link__Import

        enum link__Purpose {
            """
            `SECURITY` features provide metadata necessary to securely resolve fields.
            """
            SECURITY

            """
            `EXECUTION` features provide metadata necessary for operation execution.
            """
            EXECUTION
        }

        enum join__Graph {
            ACCOUNTS @join__graph(name: "accounts", url: "https://accounts.demo.starstuff.dev")
            INVENTORY @join__graph(name: "inventory", url: "https://inventory.demo.starstuff.dev")
            PRODUCTS @join__graph(name: "products", url: "https://products.demo.starstuff.dev")
            REVIEWS @join__graph(name: "reviews", url: "https://reviews.demo.starstuff.dev")
        }

        type Query {
          me: User
          customer: User @test
          itf: I
        }

        type User @join__type(graph: ACCOUNTS, key: "id") {
          id: ID! @test
          name: String
        }

        interface I @join__type(graph: ACCOUNTS, key: "id") {
            id: ID! @test
            name :String
        }
        "#;
        let query = "query { me { name } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        let query = "query { itf { name } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));
    }

    #[test]
    fn join_field_requires() {
        let schema1: &str = r#"
        schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
          query: Query
        }
        directive @test on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
        directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        scalar join__FieldSet

        scalar link__Import

        enum link__Purpose {
            """
            `SECURITY` features provide metadata necessary to securely resolve fields.
            """
            SECURITY

            """
            `EXECUTION` features provide metadata necessary for operation execution.
            """
            EXECUTION
        }

        enum join__Graph {
            ACCOUNTS @join__graph(name: "accounts", url: "https://accounts.demo.starstuff.dev")
            INVENTORY @join__graph(name: "inventory", url: "https://inventory.demo.starstuff.dev")
            PRODUCTS @join__graph(name: "products", url: "https://products.demo.starstuff.dev")
            REVIEWS @join__graph(name: "reviews", url: "https://reviews.demo.starstuff.dev")
        }

        type Query {
          me: User
          customer: User
          itf: I
        }

        type User @join__type(graph: ACCOUNTS, key: "id") {
          id: ID!
          name: String
          username: String @join__field(graph:ACCOUNTS, requires: "name")
          a: String @join__field(graph:ACCOUNTS, requires: "itf { ... on A { name } }")
          itf: I
        }

        interface I @join__type(graph: ACCOUNTS, key: "id") {
            id: ID!
            name: String
        }

        type A implements I @join__type(graph: ACCOUNTS, key: "id") {
            id: ID!
            name: String
        }
        "#;

        let schema2: &str = r#"
        schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
            query: Query
        }
        directive @test on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
        directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        scalar join__FieldSet

        scalar link__Import

        enum link__Purpose {
            """
            `SECURITY` features provide metadata necessary to securely resolve fields.
            """
            SECURITY

            """
            `EXECUTION` features provide metadata necessary for operation execution.
            """
            EXECUTION
        }

        enum join__Graph {
            ACCOUNTS @join__graph(name: "accounts", url: "https://accounts.demo.starstuff.dev")
            INVENTORY @join__graph(name: "inventory", url: "https://inventory.demo.starstuff.dev")
            PRODUCTS @join__graph(name: "products", url: "https://products.demo.starstuff.dev")
            REVIEWS @join__graph(name: "reviews", url: "https://reviews.demo.starstuff.dev")
        }

        type Query {
          me: User
          customer: User @test
          itf: I
        }

        type User @join__type(graph: ACCOUNTS, key: "id") {
          id: ID!
          name: String @test
          username: String @join__field(graph:ACCOUNTS, requires: "name")
          a: String @join__field(graph:ACCOUNTS, requires: "itf { ... on A { name } }")
          itf: I
        }

        interface I @join__type(graph: ACCOUNTS, key: "id") {
            id: ID!
            name: String @test
        }

        type A implements I @join__type(graph: ACCOUNTS, key: "id") {
            id: ID!
            name: String @test
        }
        "#;
        let query = "query { me { username } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));

        let query = "query { me { a } }";
        assert_ne!(hash(schema1, query), hash(schema2, query));
    }
}
