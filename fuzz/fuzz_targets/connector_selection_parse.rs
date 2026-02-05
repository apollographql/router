#![no_main]

use std::sync::LazyLock;

use apollo_federation::connectors::JSONSelection;
use bnf::CoverageGuided;
use bnf::Grammar;
use libfuzzer_sys::arbitrary;
use libfuzzer_sys::arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use libfuzzer_sys::Corpus;

/// Generations per fuzz input. CoverageGuided prefers grammar productions
/// not yet exercised; multiple generations let that coverage accumulate.
const GENERATIONS_PER_INPUT: usize = 8;

fuzz_target!(|input: GeneratedSelections| -> Corpus {
    if input.0.is_empty() {
        return Corpus::Reject;
    }

    for selection in &input.0 {
        let parsed = JSONSelection::parse(selection).unwrap();
        drop(parsed);
    }

    Corpus::Keep
});

const BNF_GRAMMAR: &str = r##"
        <JSONSelection>         ::= "" | <PathSelection> | <NamedSelections>
        <NamedSelections>       ::= <NamedSelection> | <NamedSelection> " " <NamedSelections>
        <SubSelection>          ::= "{}" | "{ " <NamedSelections> " }"

        <PathSelection>         ::= <Path> | <Path> " " <SubSelection>
        <Path>                  ::= <VarPath> | <KeyPath> | <AtPath> | <ExprPath>
        <PathSteps>             ::= <PathStep> | <PathStep> <PathSteps>
        <PathStep>              ::= "." <Key> | "->" <Identifier> | "->" <Identifier> <MethodArgs>
        <AtPath>                ::= "@" | "@" <PathSteps>
        <ExprPath>              ::= "$(" <LitExpr> ")" | "$(" <LitExpr> ")" <PathSteps>
        <KeyPath>               ::= <Key> <PathSteps>
        <VarPath>               ::= "$" | "$" <Identifier> | "$" <PathSteps> | "$" <Identifier> <PathSteps>
        <MethodArgs>            ::= "()" | "(" <LitExprs> ")"
        <LitExprs>              ::= <LitExpr> | <LitExpr> ", " <LitExprs>
        <LitExpr>               ::= <LitPrimitive> | <LitObject> | <LitArray> | <PathSelection>

        <LitPrimitive>          ::= <LitString> | <LitNumber> | "true" | "false" | "null"
        <LitObject>             ::= "{}" | "{" <LitProperties> "}"
        <LitArray>              ::= "[]" | "[" <LitExprs> "]"

        <LitProperties>         ::= <LitProperty> | <LitProperty> ", " <LitProperties>
        <LitProperty>           ::= <Key> ": " <LitExpr>

        <LitNumber>             ::= <Number> | "-" <Number>
        <Number>                ::= "." <Digits> | <Digits> | <Digits> "." | <Digits> "." <Digits>

        <LitString>             ::= '""' | "''" | '"' <DoubleString> '"' | "'" <SingleString> "'"
        <DoubleString>          ::= '\"' | <ASCII> | '\"' <DoubleString> | <ASCII> <DoubleString>
        <SingleString>          ::= "\'" | <ASCII> | "\'" <SingleString> | <ASCII> <SingleString>
        <ASCII>                 ::= <Letter> | <Digit> |
                                    "!" | "{" | "}" | "[" | "]" | "@" | "#" | "$" | "%" | "^" |
                                    "&" | "*" | "(" | ")" | "-" | "_" | "=" | "+" | ";" | ":" |
                                    "|" | "," | "<" | "." | ">" | "/" | "?" | " " | "\\"

        <NamedSelection>        ::= <NamedPathSelection> | <PathWithSubSelection> | <NamedFieldSelection> | <NamedGroupSelection>
        <NamedFieldSelection>   ::= <Key> | <Alias> " " <Key> | <Key> " " <SubSelection> | <Alias> " " <Key> " " <SubSelection>
        <NamedGroupSelection>   ::= <Alias> " " <SubSelection>

        <NamedPathSelection>    ::= <Alias> " " <PathSelection>
        <PathWithSubSelection>  ::= <Path> " " <SubSelection>

        <Alias>                 ::= <Key> ":"
        <Key>                   ::= <Identifier>
        <Identifier>            ::= <Letter> | <Letter> <LetterOrDigits>
        <Lowercase>             ::= "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" |
                                    "k" | "l" | "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" |
                                    "u" | "v" | "w" | "x" | "y" | "z"
        <Uppercase>             ::= "A" | "B" | "C" | "D" | "E" | "F" | "G" | "H" | "I" | "J" |
                                    "K" | "L" | "M" | "N" | "O" | "P" | "Q" | "R" | "S" | "T" |
                                    "U" | "V" | "W" | "X" | "Y" | "Z"
        <Letter>                ::= <Lowercase> | <Uppercase>
        <Digit>                 ::= "0" | <NonZero>
        <NonZero>               ::= "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
        <Digits>                ::= <Digit> | <Digit> <Digits>
        <LetterOrDigit>         ::= <Letter> | <Digit>
        <LetterOrDigits>        ::= <LetterOrDigit> | <LetterOrDigit> <LetterOrDigits>
    "##;
static GRAMMAR: LazyLock<Grammar> = LazyLock::new(|| BNF_GRAMMAR.parse().unwrap());

/// One fuzz input: a seed produces multiple grammar-generated strings via
/// CoverageGuided, which prefers productions not yet used so we exercise
/// more of the grammar per input.
struct GeneratedSelections(Vec<String>);
impl<'a> Arbitrary<'a> for GeneratedSelections {
    fn arbitrary(u: &mut libfuzzer_sys::arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let bytes = <[u8; 32] as Arbitrary>::arbitrary(u)?;
        let mut strategy = CoverageGuided::from_seed(bytes);

        let selections: Vec<String> = (0..GENERATIONS_PER_INPUT)
            .filter_map(|_| GRAMMAR.generate_seeded_with_strategy(&mut strategy).ok())
            .collect();
        Ok(GeneratedSelections(selections))
    }
}

impl std::fmt::Debug for GeneratedSelections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for selection in &self.0 {
            write!(f, "```original\n{}\n```", selection)?;
            if let Ok(parsed) = JSONSelection::parse(selection) {
                write!(f, "\n\n```pretty\n{}\n```", parsed)?;
            }
        }
        Ok(())
    }
}
