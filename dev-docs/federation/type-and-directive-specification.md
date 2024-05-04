## Type and Directive Specification

Source code: [src/schema/type_and_directive_specification.rs](../src/schema/type_and_directive_specification.rs)

### Overview

The type and directive specification structs are used to add such definitions into a schema in a programmatic fashion. They also performs validation. This system is used in both composition and query planning.

 - During composition, each parsed subgraph schema is incrementally added to the composed supergraph schema by constructing their type and directive specifications.
 - For the preparation of query planning, the loaded supergraph schema gets decomposed into subgraphs, during which some built-in subgraph constructs are added via type and directive specifications.

### TypeAndDirectiveSpecification trait

For each type and directive schema definition kind, there is a corresponding specification struct in Rust implementation. Those specification types are

- `ScalarTypeSpecification`
- `EnumTypeSpecification`
- `UnionTypeSpecification`
- `ObjectTypeSpecification`
- `DirectiveSpecification`

Notably, `InputTypeSpecification` is missing, because bootstrapping code would be more complex with it (see [this Slack discussion](https://apollograph.slack.com/archives/C05263RUETS/p1714067685636049) for context).

Those specification structs implement the `TypeAndDirectiveSpecification` trait and it has one method:
```rust
    fn check_or_add(&self, schema: &mut FederationSchema) -> Result<(), FederationError>;
```

As the name suggests, `check_or_add` validates composition and adds the definition into the schema object provided.

### Validation rules

The implementation encodes the composition validation rules. If the new specification has an existing definition in the schema, it won't change the schema. However, it gets validated to be compatible with the existing definition. The validation rules are following:

- `ScalarTypeSpecification`
    - Existing type definition must also be a scalar type.
- `EnumTypeSpecification`
    - Existing type definition must also be an enum type.
    - Both definitions must have the same set of values (names).
- `UnionTypeSpecification`
    - Existing type definition must also be a union type.
    - They must have the same set of union members.
    - Note: Union types must have at least one member.
- `ObjectTypeSpecification`
    - Existing type definition must also be an object type.
    - They must have same/compatible set of fields as defined as following:
        - All of new definition's fields are a subset of the existing definition's fields.
        - Each pair of matching fields (having the same field name) must have the same type and same arguments.
        - Note: The original JS implementation allowed the new definition to have fewer fields. But, it's unclear if it is a bug or not (no comments on this in the code).
    - Having same arguments is defined as following:
        - Both field definitions must have the exactly same set of argument names.
        - Each pair of matching arguments have the same type.
    - Having same argument type is defined as following:
        - Both arguments have the same type with the following exceptions:
            - New definition's argument is allowed to be non-nullable type `T!` if the existing definition has nullable type `T`.
            - Some redefinition is allowed if the root named type of existing type is a customer scalar and that of the new definition is a non-custom scalar. For example, `arg1: [DateTime]!` can be redefined as `arg1: [String]!`.
        - If both definitions have nullable types, their default values must be the same.
- `DirectiveSpecification`
    - Both definitions must have the same arguments (as defined for fields above).
    - If existing definition is repeatable, it's ok for the new definition is non-repeatable.
    - New definition's locations must be a subset of that of existing definition. It's ok to use fewer locations in the new definition.
