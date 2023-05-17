use apollo_compiler::ast::*;
use apollo_compiler::Node;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum AstNode {
    OperationDefinition(Node<OperationDefinition>),
    FragmentDefinition(Node<FragmentDefinition>),
    VariableDefinition(Node<VariableDefinition>),
    Field(Node<Field>),
    FragmentSpread(Node<FragmentSpread>),
    InlineFragmentSpread(Node<InlineFragment>),
    Directive(Node<Directive>),
    Argument(Node<Argument>),
    Type(Node<Type>),
    Value(Node<Value>),
    DirectiveDefinition(Node<DirectiveDefinition>),
    SchemaDefinition(Node<SchemaDefinition>),
    ScalarTypeDefinition(Node<ScalarTypeDefinition>),
    ObjectTypeDefinition(Node<ObjectTypeDefinition>),
    InterfaceTypeDefinition(Node<InterfaceTypeDefinition>),
    UnionTypeDefinition(Node<UnionTypeDefinition>),
    EnumTypeDefinition(Node<EnumTypeDefinition>),
    InputObjectTypeDefinition(Node<InputObjectTypeDefinition>),
    SchemaExtension(Node<SchemaExtension>),
    ScalarTypeExtension(Node<ScalarTypeExtension>),
    ObjectTypeExtension(Node<ObjectTypeExtension>),
    InterfaceTypeExtension(Node<InterfaceTypeExtension>),
    UnionTypeExtension(Node<UnionTypeExtension>),
    EnumTypeExtension(Node<EnumTypeExtension>),
    InputObjectTypeExtension(Node<InputObjectTypeExtension>),
    FieldDefinition(Node<FieldDefinition>),
    InputValueDefinition(Node<InputValueDefinition>),
    EnumValueDefinition(Node<EnumValueDefinition>),
}
