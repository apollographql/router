//! Schema aware query hashing algorithm
//!
//! This is a query visitor that calculates a hash of all fields, along with all
//! the relevant types and directives in the schema. It is designed to generate
//! the same hash for the same query across schema updates if the schema change
//! would not affect that query. As an example, if a new type is added to the
//! schema, we know that it will have no impact to an existing query that cannot
//! be using it.
//! This algorithm is used in 2 places:
//! * in the query planner cache: generating query plans can be expensive, so the
//! router has a warm up feature, where upon receving a new schema, it will take
//! the most used queries and plan them, before switching traffic to the new
//! schema. Generating all of those plans takes a lot of time. By using this
//! hashing algorithm, we can detect that the schema change does not affect the
//! query, which means that we can reuse the old query plan directly and avoid
//! the expensive planning task
//! * in entity caching: the responses returned by subgraphs can change depending
//! on the schema (example: a field moving from String to Int), so we need to
//! detect that. One way to do it was to add the schema hash to the cache key, but
//! as a result it wipes the cache on every schema update, which will cause
//! performance and reliability issues. With this hashing algorithm, cached entries
//! can be kept across schema updates
//!
//! ## Technical details
//!
//! ### Query string hashing
//! A full hash of the query string is added along with the schema level data. This
//! is technically making the algorithm less useful, because the same query with
//! different indentation would get a different hash, while there would be no difference
//! in the query plan or the subgraph response. But this makes sure that if we forget
//! something in the way we hash the query, we will avoid collisions.
//!
//! ### Prefixes and suffixes
//! Across the entire visitor, we add prefixes and suffixes like this:
//!
//! ```rust
//! "^SCHEMA".hash(self);
//! ```
//!
//! This prevents possible collision while hashing multiple things in a sequence. The
//! `^` character cannot be present in a GraphQL query or schema outside of comments
//! or strings, so this is a good separator.
use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::Hash;
use std::hash::Hasher;

use apollo_compiler::ast;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::executable;
use apollo_compiler::parser::Parser;
use apollo_compiler::schema;
use apollo_compiler::schema::DirectiveList;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InterfaceType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Node;
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
    // TODO: remove once introspection has been moved out of query planning
    // For now, introspection is still handled by the planner, so when an
    // introspection query is hashed, it should take the whole schema into account
    schema_str: &'a str,
    hasher: Sha256,
    fragments: HashMap<&'a Name, &'a Node<executable::Fragment>>,
    hashed_types: HashSet<String>,
    seen_introspection: bool,
    join_field_directive_name: Option<String>,
    join_type_directive_name: Option<String>,
}

impl<'a> QueryHashVisitor<'a> {
    pub(crate) fn new(
        schema: &'a schema::Schema,
        schema_str: &'a str,
        executable: &'a executable::ExecutableDocument,
    ) -> Result<Self, BoxError> {
        let mut visitor = Self {
            schema,
            schema_str,
            hasher: Sha256::new(),
            fragments: executable.fragments.iter().collect(),
            hashed_types: HashSet::new(),
            seen_introspection: false,
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
        };

        visitor.hash_schema()?;

        Ok(visitor)
    }

    pub(crate) fn hash_schema(&mut self) -> Result<(), BoxError> {
        "^SCHEMA".hash(self);
        for directive_definition in self.schema.directive_definitions.values() {
            self.hash_directive_definition(directive_definition)?;
        }
        for directive in &self.schema.schema_definition.directives {
            self.hash_directive(directive);
        }
        "^SCHEMA-END".hash(self);
        Ok(())
    }

    pub(crate) fn hash_query(
        schema: &'a schema::Schema,
        schema_str: &'a str,
        executable: &'a executable::ExecutableDocument,
        operation_name: Option<&str>,
    ) -> Result<Vec<u8>, BoxError> {
        let mut visitor = QueryHashVisitor::new(schema, schema_str, executable)?;
        traverse::document(&mut visitor, executable, operation_name)?;
        // hash the entire query string to prevent collisions
        executable.to_string().hash(&mut visitor);
        Ok(visitor.finish())
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.hasher.finalize().as_slice().into()
    }

    fn hash_directive_definition(
        &mut self,
        directive_definition: &Node<ast::DirectiveDefinition>,
    ) -> Result<(), BoxError> {
        "^DIRECTIVE_DEFINITION".hash(self);
        directive_definition.name.as_str().hash(self);
        for argument in &directive_definition.arguments {
            self.hash_input_value_definition(argument)?;
        }
        "^DIRECTIVE_DEFINITION-END".hash(self);

        Ok(())
    }

    fn hash_directive(&mut self, directive: &Node<ast::Directive>) {
        "^DIRECTIVE".hash(self);
        directive.name.as_str().hash(self);
        for argument in &directive.arguments {
            self.hash_argument(argument)
        }
        "^DIRECTIVE-END".hash(self);
    }

    fn hash_argument(&mut self, argument: &Node<ast::Argument>) {
        "^ARGUMENT".hash(self);
        argument.name.hash(self);
        self.hash_value(&argument.value);
        "^ARGUMENT-END".hash(self);
    }

    fn hash_value(&mut self, value: &ast::Value) {
        "^VALUE".hash(self);

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
                "^list[".hash(self);
                for v in l.iter() {
                    self.hash_value(v);
                }
                "^]".hash(self);
            }
            schema::Value::Object(o) => {
                "^object{".hash(self);
                for (k, v) in o.iter() {
                    "^key".hash(self);

                    k.hash(self);
                    "^:".hash(self);
                    self.hash_value(v);
                }
                "^}".hash(self);
            }
        }
        "^VALUE-END".hash(self);
    }

    fn hash_type_by_name(&mut self, name: &str) -> Result<(), BoxError> {
        "^TYPE_BY_NAME".hash(self);

        name.hash(self);

        // we need this this to avoid an infinite loop when hashing types that refer to each other
        if self.hashed_types.contains(name) {
            return Ok(());
        }

        self.hashed_types.insert(name.to_string());

        if let Some(ty) = self.schema.types.get(name) {
            self.hash_extended_type(ty)?;
        }
        "^TYPE_BY_NAME-END".hash(self);

        Ok(())
    }

    fn hash_extended_type(&mut self, t: &'a ExtendedType) -> Result<(), BoxError> {
        "^EXTENDED_TYPE".hash(self);

        match t {
            ExtendedType::Scalar(s) => {
                "^SCALAR".hash(self);

                for directive in &s.directives {
                    self.hash_directive(&directive.node);
                }
            }
            // this only hashes the type level info, not the fields, because those will be taken from the query
            // we will still hash the fields using for the key
            ExtendedType::Object(o) => {
                "^OBJECT".hash(self);

                for directive in &o.directives {
                    self.hash_directive(&directive.node);
                }

                self.hash_join_type(&o.name, &o.directives)?;
                //FIXME: hash implemented interfaces?
            }
            ExtendedType::Interface(i) => {
                "^INTERFACE".hash(self);

                for directive in &i.directives {
                    self.hash_directive(&directive.node);
                }
                self.hash_join_type(&i.name, &i.directives)?;
                //FIXME: hash implemented interfaces?
            }
            ExtendedType::Union(u) => {
                "^UNION".hash(self);

                for directive in &u.directives {
                    self.hash_directive(&directive.node);
                }

                for member in &u.members {
                    self.hash_type_by_name(member.as_str())?;
                }
            }
            ExtendedType::Enum(e) => {
                "^ENUM".hash(self);

                for directive in &e.directives {
                    self.hash_directive(&directive.node);
                }

                for (value, def) in &e.values {
                    "^VALUE".hash(self);

                    value.hash(self);
                    for directive in &def.directives {
                        self.hash_directive(directive);
                    }
                }
            }
            ExtendedType::InputObject(o) => {
                "^INPUT_OBJECT".hash(self);

                for directive in &o.directives {
                    self.hash_directive(&directive.node);
                }

                for (name, ty) in &o.fields {
                    "^KEY".hash(self);

                    //FIXME: always hash the name?
                    if ty.default_value.is_some() {
                        name.hash(self);
                        self.hash_input_value_definition(&ty.node)?;
                    }
                }
            }
        }
        "^EXTENDED_TYPE-END".hash(self);

        Ok(())
    }

    fn hash_type(&mut self, t: &ast::Type) -> Result<(), BoxError> {
        "^TYPE".hash(self);

        match t {
            schema::Type::Named(name) => self.hash_type_by_name(name.as_str())?,
            schema::Type::NonNullNamed(name) => {
                "!".hash(self);
                self.hash_type_by_name(name.as_str())?;
            }
            schema::Type::List(t) => {
                "[]".hash(self);
                self.hash_type(t)?;
            }
            schema::Type::NonNullList(t) => {
                "[]!".hash(self);
                self.hash_type(t)?;
            }
        }
        "^TYPE-END".hash(self);
        Ok(())
    }

    fn hash_field(
        &mut self,
        parent_type: String,
        field_def: &FieldDefinition,
        node: &executable::Field,
    ) -> Result<(), BoxError> {
        "^FIELD".hash(self);

        self.hash_type_by_name(&parent_type)?;

        field_def.name.hash(self);
        self.hash_type(&field_def.ty)?;

        for argument in &field_def.arguments {
            self.hash_input_value_definition(argument)?;
        }

        for argument in &node.arguments {
            self.hash_argument(argument);
        }

        self.hash_type(&field_def.ty)?;

        for directive in &field_def.directives {
            self.hash_directive(directive);
        }

        // for every field, we also need to look at fields defined in `@requires` because
        // they will affect the query plan
        self.hash_join_field(&parent_type, &field_def.directives)?;

        for directive in &node.directives {
            self.hash_directive(directive);
        }

        node.alias.hash(self);
        "^FIELD-END".hash(self);

        Ok(())
    }

    fn hash_input_value_definition(
        &mut self,
        t: &Node<ast::InputValueDefinition>,
    ) -> Result<(), BoxError> {
        "^INPUT_VALUE".hash(self);

        self.hash_type(&t.ty)?;
        for directive in &t.directives {
            self.hash_directive(directive);
        }
        if let Some(value) = t.default_value.as_ref() {
            self.hash_value(value);
        } else {
            "^INPUT_VALUE-NO_DEFAULT".hash(self);
        }
        "^INPUT_VALUE-END".hash(self);
        Ok(())
    }

    fn hash_join_type(&mut self, name: &Name, directives: &DirectiveList) -> Result<(), BoxError> {
        "^JOIN_TYPE".hash(self);

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
        "^JOIN_TYPE-END".hash(self);

        Ok(())
    }

    fn hash_join_field(
        &mut self,
        parent_type: &str,
        directives: &ast::DirectiveList,
    ) -> Result<(), BoxError> {
        "^JOIN_FIELD".hash(self);

        if let Some(dir_name) = self.join_field_directive_name.as_deref() {
            if let Some(dir) = directives.get(dir_name) {
                if let Some(requires) = dir
                    .argument_by_name("requires")
                    .and_then(|arg| arg.as_str())
                {
                    if let Ok(parent_type) = Name::new(parent_type) {
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
        "^JOIN_FIELD-END".hash(self);

        Ok(())
    }

    fn hash_interface_implementers(
        &mut self,
        intf: &InterfaceType,
        node: &executable::Field,
    ) -> Result<(), BoxError> {
        "^INTERFACE_IMPL".hash(self);

        if let Some(implementers) = self.schema.implementers_map().get(&intf.name) {
            for object in &implementers.objects {
                self.hash_type_by_name(object)?;
                traverse::selection_set(self, object, &node.selection_set.selections)?;
            }
        }

        "^INTERFACE_IMPL-END".hash(self);
        Ok(())
    }
}

impl<'a> Hasher for QueryHashVisitor<'a> {
    fn finish(&self) -> u64 {
        unreachable!()
    }

    fn write(&mut self, bytes: &[u8]) {
        // byte separator between each part that is hashed
        self.hasher.update(&[0xFF][..]);
        self.hasher.update(bytes);
    }
}

impl<'a> Visitor for QueryHashVisitor<'a> {
    fn operation(&mut self, root_type: &str, node: &executable::Operation) -> Result<(), BoxError> {
        "^VISIT_OPERATION".hash(self);

        root_type.hash(self);
        self.hash_type_by_name(root_type)?;
        node.operation_type.hash(self);
        node.name.hash(self);
        for variable in &node.variables {
            variable.name.hash(self);
            self.hash_type(&variable.ty)?;

            if let Some(value) = variable.default_value.as_ref() {
                self.hash_value(value);
            } else {
                "^VISIT_OPERATION-NO_DEFAULT".hash(self);
            }

            for directive in &variable.directives {
                self.hash_directive(directive);
            }
        }
        for directive in &node.directives {
            self.hash_directive(directive);
        }

        traverse::operation(self, root_type, node)?;
        "^VISIT_OPERATION-END".hash(self);
        Ok(())
    }

    fn field(
        &mut self,
        parent_type: &str,
        field_def: &ast::FieldDefinition,
        node: &executable::Field,
    ) -> Result<(), BoxError> {
        "^VISIT_FIELD".hash(self);

        if !self.seen_introspection && (field_def.name == "__schema" || field_def.name == "__type")
        {
            self.seen_introspection = true;
            self.schema_str.hash(self);
        }

        self.hash_field(parent_type.to_string(), field_def, node)?;

        if let Some(ExtendedType::Interface(intf)) =
            self.schema.types.get(field_def.ty.inner_named_type())
        {
            self.hash_interface_implementers(intf, node)?;
        }

        traverse::field(self, field_def, node)?;
        "^VISIT_FIELD_END".hash(self);
        Ok(())
    }

    fn fragment(&mut self, node: &executable::Fragment) -> Result<(), BoxError> {
        "^VISIT_FRAGMENT".hash(self);

        node.name.hash(self);
        self.hash_type_by_name(node.type_condition())?;
        for directive in &node.directives {
            self.hash_directive(directive);
        }

        traverse::fragment(self, node)?;
        "^VISIT_FRAGMENT-END".hash(self);

        Ok(())
    }

    fn fragment_spread(&mut self, node: &executable::FragmentSpread) -> Result<(), BoxError> {
        "^VISIT_FRAGMENT_SPREAD".hash(self);

        node.fragment_name.hash(self);
        let type_condition = &self
            .fragments
            .get(&node.fragment_name)
            .ok_or("MissingFragment")?
            .type_condition();
        self.hash_type_by_name(type_condition)?;

        for directive in &node.directives {
            self.hash_directive(directive);
        }

        traverse::fragment_spread(self, node)?;
        "^VISIT_FRAGMENT_SPREAD-END".hash(self);

        Ok(())
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        node: &executable::InlineFragment,
    ) -> Result<(), BoxError> {
        "^VISIT_INLINE_FRAGMENT".hash(self);

        if let Some(type_condition) = &node.type_condition {
            self.hash_type_by_name(type_condition)?;
        }
        for directive in &node.directives {
            self.hash_directive(directive);
        }

        traverse::inline_fragment(self, parent_type, node)?;
        "^VISIT_INLINE_FRAGMENT-END".hash(self);
        Ok(())
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

    #[derive(Debug)]
    struct HashComparator {
        from_visitor: String,
        from_hash_query: String,
    }

    impl From<(String, String)> for HashComparator {
        fn from(value: (String, String)) -> Self {
            Self {
                from_visitor: value.0,
                from_hash_query: value.1,
            }
        }
    }

    // The non equality check is not the same
    // as one would expect from PartialEq.
    // This is why HashComparator doesn't implement it.
    impl HashComparator {
        fn equals(&self, other: &Self) -> bool {
            self.from_visitor == other.from_visitor && self.from_hash_query == other.from_hash_query
        }
        fn doesnt_match(&self, other: &Self) -> bool {
            // This is intentional, we check to prevent BOTH hashes from being equal
            self.from_visitor != other.from_visitor && self.from_hash_query != other.from_hash_query
        }
    }

    #[track_caller]
    fn hash(schema_str: &str, query: &str) -> HashComparator {
        let schema = Schema::parse(schema_str, "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let doc = Document::parse(query, "query.graphql").unwrap();

        let exec = doc
            .to_executable(&schema)
            .unwrap()
            .validate(&schema)
            .unwrap();
        let mut visitor = QueryHashVisitor::new(&schema, schema_str, &exec).unwrap();
        traverse::document(&mut visitor, &exec, None).unwrap();

        (
            hex::encode(visitor.finish()),
            hex::encode(QueryHashVisitor::hash_query(&schema, schema_str, &exec, None).unwrap()),
        )
            .into()
    }

    #[track_caller]
    fn hash_subgraph_query(schema_str: &str, query: &str) -> String {
        let schema = Valid::assume_valid(Schema::parse(schema_str, "schema.graphql").unwrap());
        let doc = Document::parse(query, "query.graphql").unwrap();
        let exec = doc
            .to_executable(&schema)
            .unwrap()
            .validate(&schema)
            .unwrap();
        let mut visitor = QueryHashVisitor::new(&schema, schema_str, &exec).unwrap();
        traverse::document(&mut visitor, &exec, None).unwrap();

        hex::encode(visitor.finish())
    }

    #[test]
    fn me() {
        let schema1: &str = r#"
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
        type Query {
          me: User
        }
    
    
        type User {
          id: ID!
          name: String
        }
        "#;
        let query = "query { me { name } }";
        assert!(hash(schema1, query).equals(&hash(schema2, query)));

        // id is nullable in 1, non nullable in 2
        let query = "query { me { id name } }";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        // simple normalization
        let query = "query {  moi: me { name   } }";
        assert!(hash(schema1, query).equals(&hash(schema2, query)));

        assert!(hash(schema1, "query { me { id name } }")
            .doesnt_match(&hash(schema1, "query { me { name id } }")));
    }

    #[test]
    fn directive() {
        let schema1: &str = r#"
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
        assert!(hash(schema1, query).equals(&hash(schema2, query)));

        let query = "query { me { id name } }";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        let query = "query { customer { id } }";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));
    }

    #[test]
    fn interface() {
        let schema1: &str = r#"
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
        assert!(hash(schema1, query).equals(&hash(schema2, query)));

        let query = "query { customer { id } }";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        let query = "query { customer { ... on User { name } } }";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));
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
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        let query = "query { b }";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        let query = "query { b(i: 0)}";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        let query = "query { c(j: 0)}";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        let query = "query { c(i:0, j: 0)}";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));
    }

    #[test]
    fn entities() {
        let schema1: &str = r#"
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

        let hash1 = hash_subgraph_query(schema1, query1);
        let hash2 = hash_subgraph_query(schema2, query1);
        assert_ne!(hash1, hash2);

        let query2 = r#"query Query1($representations:[_Any!]!){
            _entities(representations:$representations){
                ...on User {
                    name
                }
            }
        }"#;

        let hash1 = hash_subgraph_query(schema1, query2);
        let hash2 = hash_subgraph_query(schema2, query2);
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
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        let query = "query { itf { name } }";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));
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
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));

        let query = "query { me { a } }";
        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));
    }

    #[test]
    fn introspection() {
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
          me: NotUser
        }
    
    
        type NotUser {
          id: ID!
          name: String
        }
        "#;

        let query = "{ __schema { types { name } } }";

        assert!(hash(schema1, query).doesnt_match(&hash(schema2, query)));
    }

    #[test]
    fn fields_with_different_arguments_have_different_hashes() {
        let schema: &str = r#"
        type Query {
          test(arg: Int): String
        }
        "#;

        let query_one = "query { a: test(arg: 1) b: test(arg: 2) }";
        let query_two = "query { a: test(arg: 1) b: test(arg: 3) }";

        // This assertion tests an internal hash function that isn't directly
        // used for the query hash, and we'll need to make it pass to rely
        // solely on the internal function again.
        //
        // assert!(hash(schema, query_one).doesnt_match(&hash(schema,
        // query_two)));
        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
    }

    #[test]
    fn fields_with_different_arguments_on_nest_field_different_hashes() {
        let schema: &str = r#"
        type Test {
          test(arg: Int): String
          recursiveLink: Test
        }

        type Query {
          directLink: Test
        }
        "#;

        let query_one = "{ directLink { test recursiveLink { test(arg: 1) } } }";
        let query_two = "{ directLink { test recursiveLink { test(arg: 2) } } }";

        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
        assert!(hash(schema, query_one).from_visitor != hash(schema, query_two).from_visitor);
    }

    #[test]
    fn fields_with_different_aliases_have_different_hashes() {
        let schema: &str = r#"
        type Query {
          test(arg: Int): String
        }
        "#;

        let query_one = "{ a: test }";
        let query_two = "{ b: test }";

        // This assertion tests an internal hash function that isn't directly
        // used for the query hash, and we'll need to make it pass to rely
        // solely on the internal function again.
        //
        // assert!(hash(schema, query_one).doesnt_match(&hash(schema, query_two)));
        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
    }

    #[test]
    fn operations_with_different_names_have_different_hash() {
        let schema: &str = r#"
        type Query {
          test: String
        }
        "#;

        let query_one = "query Foo { test }";
        let query_two = "query Bar { test }";

        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
        assert!(hash(schema, query_one).from_visitor != hash(schema, query_two).from_visitor);
    }

    #[test]
    fn adding_directive_on_operation_changes_hash() {
        let schema: &str = r#"
        directive @test on QUERY
        type Query {
          test: String
        }
        "#;

        let query_one = "query { test }";
        let query_two = "query @test { test }";

        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
        assert!(hash(schema, query_one).from_visitor != hash(schema, query_two).from_visitor);
    }

    #[test]
    fn order_of_variables_changes_hash() {
        let schema: &str = r#"
        type Query {
          test1(arg: Int): String
          test2(arg: Int): String
        }
        "#;

        let query_one = "query ($foo: Int, $bar: Int) {  test1(arg: $foo) test2(arg: $bar) }";
        let query_two = "query ($foo: Int, $bar: Int) { test1(arg: $bar) test2(arg: $foo) }";

        assert!(hash(schema, query_one).doesnt_match(&hash(schema, query_two)));
    }

    #[test]
    fn query_variables_with_different_types_have_different_hash() {
        let schema: &str = r#"
        type Query {
          test(arg: Int): String
        }
        "#;

        let query_one = "query ($var: Int) { test(arg: $var) }";
        let query_two = "query ($var: Int!) { test(arg: $var) }";

        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
        assert!(hash(schema, query_one).from_visitor != hash(schema, query_two).from_visitor);
    }

    #[test]
    fn query_variables_with_different_default_values_have_different_hash() {
        let schema: &str = r#"
        type Query {
          test(arg: Int): String
        }
        "#;

        let query_one = "query ($var: Int = 1) { test(arg: $var) }";
        let query_two = "query ($var: Int = 2) { test(arg: $var) }";

        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
        assert!(hash(schema, query_one).from_visitor != hash(schema, query_two).from_visitor);
    }

    #[test]
    fn adding_directive_to_query_variable_change_hash() {
        let schema: &str = r#"
        directive @test on VARIABLE_DEFINITION

        type Query {
          test(arg: Int): String
        }
        "#;

        let query_one = "query ($var: Int) { test(arg: $var) }";
        let query_two = "query ($var: Int @test) { test(arg: $var) }";

        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
        assert!(hash(schema, query_one).from_visitor != hash(schema, query_two).from_visitor);
    }

    #[test]
    fn order_of_directives_change_hash() {
        let schema: &str = r#"
        directive @foo on FIELD
        directive @bar on FIELD

        type Query {
          test(arg: Int): String
        }
        "#;

        let query_one = "{ test @foo @bar }";
        let query_two = "{ test @bar @foo }";

        assert!(hash(schema, query_one).from_hash_query != hash(schema, query_two).from_hash_query);
        assert!(hash(schema, query_one).from_visitor != hash(schema, query_two).from_visitor);
    }

    #[test]
    fn directive_argument_type_change_hash() {
        let schema1: &str = r#"
        directive @foo(a: Int) on FIELD
        directive @bar on FIELD

        type Query {
          test(arg: Int): String
        }
        "#;

        let schema2: &str = r#"
        directive @foo(a: Int!) on FIELD
        directive @bar on FIELD

        type Query {
          test(arg: Int): String
        }
        "#;

        let query = "{ test @foo(a: 1) }";

        assert!(hash(schema1, query).from_hash_query != hash(schema2, query).from_hash_query);
        assert!(hash(schema1, query).from_visitor != hash(schema2, query).from_visitor);
    }

    #[test]
    fn adding_directive_on_schema_changes_hash() {
        let schema1: &str = r#"
        schema {
          query: Query
        } 

        type Query {
          foo: String
        }
        "#;

        let schema2: &str = r#"
        directive @test on SCHEMA
        schema @test {
          query: Query
        } 

        type Query {
          foo: String
        }
        "#;

        let query = "{ foo }";

        assert!(hash(schema1, query).from_hash_query != hash(schema2, query).from_hash_query);
        assert!(hash(schema1, query).from_visitor != hash(schema2, query).from_visitor);
    }

    #[test]
    fn changing_type_of_field_changes_hash() {
        let schema1: &str = r#"
        type Query {
          test: Int
        }
        "#;

        let schema2: &str = r#"
        type Query {
          test: Float
        }
        "#;

        let query = "{ test }";

        assert!(hash(schema1, query).from_hash_query != hash(schema2, query).from_hash_query);
        assert!(hash(schema1, query).from_visitor != hash(schema2, query).from_visitor);
    }

    #[test]
    fn changing_type_to_interface_changes_hash() {
        let schema1: &str = r#"
        type Query {
          foo: Foo
        }

        interface Foo {
          value: String
        }
        "#;

        let schema2: &str = r#"
        type Query {
          foo: Foo
        }

        type Foo {
          value: String
        }
        "#;

        let query = "{ foo { value } }";

        assert!(hash(schema1, query).from_hash_query != hash(schema2, query).from_hash_query);
        assert!(hash(schema1, query).from_visitor != hash(schema2, query).from_visitor);
    }

    #[test]
    fn changing_operation_kind_changes_hash() {
        let schema: &str = r#"
        schema {
          query: Test
          mutation: Test
        }

        type Test {
          test: String
        }
        "#;

        let query_one = "query { test }";
        let query_two = "mutation { test }";

        assert_ne!(
            hash(schema, query_one).from_hash_query,
            hash(schema, query_two).from_hash_query
        );
        assert_ne!(
            hash(schema, query_one).from_visitor,
            hash(schema, query_two).from_visitor
        );
    }

    #[test]
    fn adding_directive_on_field_should_change_hash() {
        let schema: &str = r#"
        directive @test on FIELD

        type Query {
          test: String
        }
        "#;

        let query_one = "{ test }";
        let query_two = "{ test @test }";

        assert_ne!(
            hash(schema, query_one).from_hash_query,
            hash(schema, query_two).from_hash_query
        );
        assert_ne!(
            hash(schema, query_one).from_visitor,
            hash(schema, query_two).from_visitor
        );
    }

    #[test]
    fn adding_directive_on_fragment_spread_change_hash() {
        let schema: &str = r#"
        type Query {
          test: String
        }
        "#;

        let query_one = r#"
        { ...Test }

        fragment Test on Query {
          test
        }
        "#;
        let query_two = r#"
        { ...Test @skip(if: false) }

        fragment Test on Query {
          test
        }
        "#;

        assert_ne!(
            hash(schema, query_one).from_hash_query,
            hash(schema, query_two).from_hash_query
        );
        assert_ne!(
            hash(schema, query_one).from_visitor,
            hash(schema, query_two).from_visitor
        );
    }

    #[test]
    fn adding_directive_on_fragment_change_hash() {
        let schema: &str = r#"
        directive @test on FRAGMENT_DEFINITION

        type Query {
          test: String
        }
        "#;

        let query_one = r#"
        { ...Test }

        fragment Test on Query {
          test
        }
        "#;
        let query_two = r#"
        { ...Test }

        fragment Test on Query @test {
          test
        }
        "#;

        assert_ne!(
            hash(schema, query_one).from_hash_query,
            hash(schema, query_two).from_hash_query
        );
        assert_ne!(
            hash(schema, query_one).from_visitor,
            hash(schema, query_two).from_visitor
        );
    }

    #[test]
    fn adding_directive_on_inline_fragment_change_hash() {
        let schema: &str = r#"
        type Query {
          test: String
        }
        "#;

        let query_one = "{ ... { test } }";
        let query_two = "{ ... @skip(if: false) { test } }";

        assert_ne!(
            hash(schema, query_one).from_hash_query,
            hash(schema, query_two).from_hash_query
        );
        assert_ne!(
            hash(schema, query_one).from_visitor,
            hash(schema, query_two).from_visitor
        );
    }

    #[test]
    fn moving_field_changes_hash() {
        let schema: &str = r#"
        type Query {
          me: User
        }

        type User {
          id: ID
          name: String
          friend: User
        }
        "#;

        let query_one = r#"
        { 
          me {
            friend {
              id
              name
            }
          }
        }
        "#;
        let query_two = r#"
        { 
          me {
            friend {
              id
            }
            name
          }
        }
        "#;

        assert_ne!(
            hash(schema, query_one).from_hash_query,
            hash(schema, query_two).from_hash_query
        );
        assert_ne!(
            hash(schema, query_one).from_visitor,
            hash(schema, query_two).from_visitor
        );
    }

    #[test]
    fn changing_type_of_fragment_changes_hash() {
        let schema: &str = r#"
        type Query {
          fooOrBar: FooOrBar
        }

        type Foo {
          id: ID
          value: String
        }

        type Bar {
          id: ID
          value: String
        }

        union FooOrBar = Foo | Bar
        "#;

        let query_one = r#"
        { 
          fooOrBar {
            ... on Foo { id }
            ... on Bar { id }
            ... Test
          }
        }

        fragment Test on Foo {
          value
        }
        "#;
        let query_two = r#"
        { 
          fooOrBar {
            ... on Foo { id }
            ... on Bar { id }
            ... Test
          }
        }

        fragment Test on Bar {
          value
        }
        "#;

        assert_ne!(
            hash(schema, query_one).from_hash_query,
            hash(schema, query_two).from_hash_query
        );
        assert_ne!(
            hash(schema, query_one).from_visitor,
            hash(schema, query_two).from_visitor
        );
    }

    #[test]
    fn changing_interface_implementors_changes_hash() {
        let schema1: &str = r#"
        type Query {
            data: I
        }

        interface I {
            id: ID
            value: String
        }

        type Foo implements I {
          id: ID
          value: String
          foo: String
        }

        type Bar {
          id: ID
          value: String
          bar: String
        }
        "#;

        let schema2: &str = r#"
        type Query {
            data: I
        }

        interface I {
            id: ID
            value: String
        }

        type Foo implements I {
          id: ID
          value: String
          foo2: String
        }

        type Bar {
          id: ID
          value: String
          bar: String
        }
        "#;

        let schema3: &str = r#"
        type Query {
            data: I
        }

        interface I {
            id: ID
            value: String
        }

        type Foo implements I {
          id: ID
          value: String
          foo: String
        }

        type Bar implements I {
          id: ID
          value: String
          bar: String
        }
        "#;

        let query = r#"
        {
          data {
            id
            value
          }
        }
        "#;

        // changing an unrelated field in implementors does not change the hash
        assert_eq!(
            hash(schema1, query).from_hash_query,
            hash(schema2, query).from_hash_query
        );
        assert_eq!(
            hash(schema1, query).from_visitor,
            hash(schema2, query).from_visitor
        );

        // adding a new implementor changes the hash
        assert_ne!(
            hash(schema1, query).from_hash_query,
            hash(schema3, query).from_hash_query
        );
        assert_ne!(
            hash(schema1, query).from_visitor,
            hash(schema3, query).from_visitor
        );
    }

    #[test]
    fn changing_interface_directives_changes_hash() {
        let schema1: &str = r#"
        directive @a(name: String) on INTERFACE

        type Query {
            data: I
        }

        interface I @a {
            id: ID
            value: String
        }

        type Foo implements I {
          id: ID
          value: String
          foo: String
        }
        "#;

        let schema2: &str = r#"
        directive @a(name: String) on INTERFACE

        type Query {
            data: I
        }

        interface I  @a(name: "abc") {
            id: ID
            value: String
        }

        type Foo implements I {
          id: ID
          value: String
          foo2: String
        }

        "#;

        let query = r#"
        {
          data {
            id
            value
          }
        }
        "#;

        // changing a directive applied on the interface definition changes the hash
        assert_ne!(
            hash(schema1, query).from_hash_query,
            hash(schema2, query).from_hash_query
        );
        assert_ne!(
            hash(schema1, query).from_visitor,
            hash(schema2, query).from_visitor
        );
    }

    #[test]
    fn it_is_weird_so_i_dont_know_how_to_name_it_change_hash() {
        let schema: &str = r#"
        type Query {
          id: ID
          someField: SomeType
          test: String
        }

        type SomeType {
          id: ID
          test: String
        }
        "#;

        let query_one = r#"
        {
          test 
          someField { id test }
          id
        }
        "#;
        let query_two = r#"
        { 
          ...test
          someField { id }
        }

        fragment test on Query {
          id
        }
        "#;

        assert_ne!(
            hash(schema, query_one).from_hash_query,
            hash(schema, query_two).from_hash_query
        );
        assert_ne!(
            hash(schema, query_one).from_visitor,
            hash(schema, query_two).from_visitor
        );
    }
}
