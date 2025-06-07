// Macro expansion tests using macrotest
// These tests verify that the RouterError derive macro generates the correct code

#[test]
fn test_macro_expansion() {
    macrotest::expand("tests/expand/*.rs");
} 