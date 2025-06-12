use super::lexer::{Token, tokenize};
use codespan_reporting::diagnostic::Label;
use codespan_reporting::diagnostic::Severity;

pub fn aol_parse(source: &str) -> Result<ParsedAol<'_>, Vec<Diagnostic>> {
    let mut diags = vec![];
    let cst = Parser::parse(source, &mut diags);
    let errors = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .cloned()
        .collect::<Vec<_>>();

    if !errors.is_empty() {
        return Err(errors);
    }
    let warnings = diags
        .iter()
        .filter(|d| d.severity != Severity::Error)
        .cloned()
        .collect::<Vec<_>>();
    let warnings = if warnings.is_empty() {
        None
    } else {
        Some(warnings)
    };

    Ok(ParsedAol { cst, warnings })
}

#[derive(Debug, PartialEq)]
pub struct ParsedAol<'parsed> {
    pub cst: Cst<'parsed>,
    /// Any warnings encountered during parsing
    pub warnings: Option<Vec<Diagnostic>>,
}

// TODO: change definition and all uses if codespan_reporting is not used
pub type Diagnostic = codespan_reporting::diagnostic::Diagnostic<()>;

// TODO: add context information to the parser if required
#[derive(Default)]
pub struct Context<'a> {
    marker: std::marker::PhantomData<&'a ()>,
}

include!(concat!(env!("OUT_DIR"), "/generated.rs"));

impl ParserCallbacks for Parser<'_> {
    fn create_tokens(source: &str, diags: &mut Vec<Diagnostic>) -> (Vec<Token>, Vec<Span>) {
        tokenize(source, diags)
    }
    fn create_diagnostic(&self, span: Span, message: String) -> Diagnostic {
        Diagnostic::error()
            .with_message(message)
            .with_labels(vec![Label::primary((), span)])
    }
}

impl PartialEq for CstIndex {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Rule(l0, l1), Self::Rule(r0, r1)) => l0 == r0 && l1 == r1,
            (Self::Token(l0, l1), Self::Token(r0, r1)) => l0 == r0 && l1 == r1,
            _ => false,
        }
    }
}

impl PartialEq for Cst<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
            && self.spans == other.spans
            && self.nodes == other.nodes
            && self.token_count == other.token_count
            && self.non_skip_len == other.non_skip_len
    }
}

impl std::fmt::Debug for Cst<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cst")
            .field("source", &self.source)
            .field("spans", &self.spans)
            .field("nodes", &self.nodes)
            .field("token_count", &self.token_count)
            .field("non_skip_len", &self.non_skip_len)
            .finish()
    }
}
