pub mod connect;

pub mod to_remove {
    use std::fmt::Display;
    use std::fmt::Formatter;

    use crate::sources::connect::ConnectId;

    #[derive(
        Debug, Clone, Copy, Hash, PartialEq, Eq, strum_macros::Display, strum_macros::EnumIter,
    )]
    pub enum SourceKind {
        #[strum(to_string = "Connect")]
        Connect,
    }

    #[derive(Debug, Clone, Hash, PartialEq, Eq, derive_more::From)]
    pub enum SourceId {
        Connect(ConnectId),
    }

    impl SourceId {
        pub(crate) fn kind(&self) -> SourceKind {
            match self {
                SourceId::Connect(_) => SourceKind::Connect,
            }
        }
    }

    impl Display for SourceId {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            let kind = self.kind();
            match self {
                SourceId::Connect(id) => {
                    write!(f, "{kind}:{id}")
                }
            }
        }
    }

    pub mod connect {
        use apollo_compiler::ast::Name;
        use indexmap::IndexMap;
        use serde_json_bytes::Value;

        use crate::sources::connect::ConnectId;
        use crate::sources::connect::JSONSelection;

        #[derive(Debug, Clone, PartialEq)]
        pub struct FetchNode {
            pub source_id: ConnectId,
            pub field_response_name: Name,              // aliasing
            pub field_arguments: IndexMap<Name, Value>, // req
            pub selection: JSONSelection,               // res
        }
    }

    #[derive(Debug, Clone, PartialEq, derive_more::From)]
    pub enum FetchNode {
        Connect(connect::FetchNode),
    }
}
