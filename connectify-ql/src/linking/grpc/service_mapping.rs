use indexmap::IndexMap;

use crate::{
    codegen::grpc::mapping_layer::{GraphQLValue, GrpcKey},
    lexer::Token,
    parser::{Cst, Node, NodeRef, Rule},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid Mapping Format")]
    InvalidFormat,
}

pub fn parse_service_mapping(
    cst: &Cst<'_>,
    source: &str,
) -> Result<IndexMap<GraphQLValue, GrpcKey>, Error> {
    let Node::Rule(Rule::Aol, _) = cst.get(NodeRef::ROOT) else {
        return Err(Error::InvalidFormat);
    };
    let children = cst
        .children(NodeRef::ROOT)
        .filter(|node_ref| {
            !matches!(
                cst.get(*node_ref),
                Node::Token(Token::Whitespace | Token::Comment | Token::Newline, _)
            )
        })
        .collect::<Vec<_>>();

    let mut map = IndexMap::new();
    for child in children.chunks(2) {
        let rpc_children = cst.children(child[0]);
        let graphql_children = cst.children(child[1]);

        let rpc_children = rpc_children
            .filter(|node_ref| {
                !matches!(
                    cst.get(*node_ref),
                    Node::Token(Token::Whitespace | Token::Comment | Token::Newline, _)
                )
            })
            .collect::<Vec<_>>();

        let graphql_children = graphql_children
            .filter(|node_ref| {
                !matches!(
                    cst.get(*node_ref),
                    Node::Token(Token::Whitespace | Token::Comment | Token::Newline, _)
                )
            })
            .collect::<Vec<_>>();
        let Some(rpc) = parse_rpc(&rpc_children, cst, source) else {
            continue;
        };
        let Some(graphql) = parse_graphql(&graphql_children, cst, source) else {
            continue;
        };

        map.insert(graphql, rpc);
    }

    Ok(map)
}

fn parse_rpc(children: &[NodeRef], cst: &Cst<'_>, source: &str) -> Option<GrpcKey> {
    let Node::Token(Token::Name, _) = cst.get(children[1]) else {
        return None;
    };
    let Node::Token(Token::Name, _) = cst.get(children[3]) else {
        return None;
    };
    let Node::Token(Token::Name, _) = cst.get(children[5]) else {
        return None;
    };

    let source_span = cst.span(children[1]);
    let source_rpc = source[source_span].to_string();

    let service_span = cst.span(children[3]);
    let service = source[service_span].to_string();

    let rpc_span = cst.span(children[5]);
    let rpc = source[rpc_span].to_string();

    Some(GrpcKey {
        source: source_rpc,
        service,
        rpc,
    })
}

fn parse_graphql(children: &[NodeRef], cst: &Cst<'_>, source: &str) -> Option<GraphQLValue> {
    let mut value = match cst.get(children[0]) {
        Node::Token(Token::Query, _) => GraphQLValue::Query(String::new()),
        Node::Token(Token::Mutation, _) => GraphQLValue::Mutation(String::new()),
        _ => return None,
    };

    let Node::Token(Token::Name, _) = cst.get(children[2]) else {
        return None;
    };

    let method_span = cst.span(children[2]);
    let method = source[method_span].to_string();

    value.set_method(method);

    Some(value)
}
