use super::parser::{Diagnostic, Span};
use codespan_reporting::diagnostic::Label;
use logos::Logos;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum LexerError {
    #[default]
    Invalid,
    // TODO: add more errors if required
}

impl LexerError {
    pub fn into_diagnostic(self, span: Span) -> Diagnostic {
        match self {
            Self::Invalid => Diagnostic::error()
                .with_message("invalid token")
                .with_labels(vec![Label::primary((), span)]),
        }
    }
}

// TODO: implement lexer
#[allow(clippy::upper_case_acronyms)]
#[derive(Logos, Debug, PartialEq, Copy, Clone)]
#[logos(error = LexerError)]
pub enum Token {
    EOF,
    #[token("use")]
    Use,
    #[token("as")]
    As,
    #[token("Mutation")]
    Mutation,
    #[token("Query")]
    Query,
    #[token(";")]
    Endline,
    #[token(".")]
    Dot,
    #[token("::")]
    Specifier,
    #[regex("[A-Za-z]{1}[A-Za-z0-9_]*[A-Za-z]?", priority = 0)]
    Name,
    #[regex("[\u{0020}\u{0009}]+")]
    Whitespace,
    #[regex("\r?\n")]
    Newline,
    #[regex("(//){2,}[\x09\x20-\x7E\u{0080}-\u{D7FF}\u{E000}-\u{10FFFF}]*")]
    Comment,
    Error,
}

// TODO: extend tokenization (e.g. check for mismatched parentheses)
pub fn tokenize(source: &str, diags: &mut Vec<Diagnostic>) -> (Vec<Token>, Vec<Span>) {
    let lexer = Token::lexer(source);
    let mut tokens = vec![];
    let mut spans = vec![];

    for (token, span) in lexer.spanned() {
        match token {
            Ok(token) => {
                tokens.push(token);
            }
            Err(err) => {
                diags.push(err.into_diagnostic(span.clone()));
                tokens.push(Token::Error);
            }
        }
        spans.push(span);
    }
    (tokens, spans)
}
