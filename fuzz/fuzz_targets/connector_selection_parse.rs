#![no_main]

use std::sync::LazyLock;

use apollo_federation::connectors::JSONSelection;
use bnf::Grammar;
use libfuzzer_sys::arbitrary;
use libfuzzer_sys::arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use libfuzzer_sys::Corpus;
use rand::rngs::StdRng;

fuzz_target!(|input: GeneratedSelection| -> Corpus {
    // Generating a selection might choose a path which recurses too deeply, so
    // we just mark those traversals as being rejected since they would require
    // seeding and iterating the Rng.
    let Some(selection) = input.0 else {
        return Corpus::Reject;
    };

    let parsed = JSONSelection::parse(&selection).unwrap();
    drop(parsed);

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

struct GeneratedSelection(Option<String>);
impl<'a> Arbitrary<'a> for GeneratedSelection {
    fn arbitrary(u: &mut libfuzzer_sys::arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let bytes = <[u8; 32] as Arbitrary>::arbitrary(u)?;
        let mut rng: StdRng = rand::SeedableRng::from_seed(bytes);

        let selection = GRAMMAR.generate_seeded(&mut rng).ok();
        Ok(GeneratedSelection(selection))
    }
}

impl std::fmt::Debug for GeneratedSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0
            .as_deref()
            .map(|selection| {
                write!(f, "```original\n{}\n```", selection)?;
                if let Ok(parsed) = JSONSelection::parse(selection) {
                    write!(f, "\n\n```pretty\n{}\n```", parsed)?;
                }

                Ok(())
            })
            .unwrap_or(Ok(()))
    }
}
