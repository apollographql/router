use apollo_compiler::Schema;
use apollo_compiler::name;

use super::SelectedValue;
use super::parse;
use super::validate::SingleSchemaOutput;
use super::validate::has_arguments;
use super::validate::validate;
use super::value::is_mappable;

#[track_caller]
fn round_trip(input: &str) -> String {
    let parsed = parse(input).expect("map should parse");
    let printed = parsed.to_string();
    // Printing is canonical: reparsing the printed form yields the same map.
    assert_eq!(
        parse(&printed).expect("printed map should reparse"),
        parsed,
        "reparse of {printed:?}"
    );
    printed
}

#[test]
fn parses_every_appendix_a_form() {
    assert_eq!(round_trip("id"), "id");
    assert_eq!(round_trip("address.id"), "address.id");
    assert_eq!(round_trip("<Book>.title"), "<Book>.title");
    assert_eq!(round_trip("mediaById<Book>.isbn"), "mediaById<Book>.isbn");
    assert_eq!(round_trip("{ width, height }"), "{ width, height }");
    assert_eq!(
        round_trip("{ w: width, h: height }"),
        "{ w: width, h: height }"
    );
    assert_eq!(
        round_trip("dimension.{ width, height }"),
        "dimension.{ width, height }"
    );
    assert_eq!(
        round_trip("dimensions[{ width, height }]"),
        "dimensions[{ width, height }]"
    );
    assert_eq!(round_trip("parts[id]"), "parts[id]");
    assert_eq!(round_trip("parts[[{ id, name }]]"), "parts[[{ id, name }]]");
    assert_eq!(
        round_trip("{ weight, dimension: dimension.{ width, height } }"),
        "{ weight, dimension: dimension.{ width, height } }"
    );
    assert_eq!(
        round_trip("{ id } | { addressId: address.id } | { name }"),
        "{ id } | { addressId: address.id } | { name }"
    );
    assert_eq!(
        round_trip("| { bookId: <Book>.id } | { movieId: <Movie>.id }"),
        "{ bookId: <Book>.id } | { movieId: <Movie>.id }"
    );
    assert_eq!(
        round_trip("{ nested: { bookId: <Book>.id } | { movieId: <Movie>.id } }"),
        "{ nested: { bookId: <Book>.id } | { movieId: <Movie>.id } }"
    );
    assert_eq!(
        round_trip("{ coordinates: coordinates[{ lat: x, lon: y }]}"),
        "{ coordinates: coordinates[{ lat: x, lon: y }] }"
    );
}

#[test]
fn parses_const_arguments() {
    assert_eq!(round_trip("width(unit: IMPERIAL)"), "width(unit: IMPERIAL)");
    assert_eq!(
        round_trip("packaging(material: BOX).weight"),
        "packaging(material: BOX).weight"
    );
    assert_eq!(
        round_trip("dimensions[{ width(unit: IMPERIAL), height(unit: IMPERIAL) }]"),
        "dimensions[{ width(unit: IMPERIAL), height(unit: IMPERIAL) }]"
    );
    assert_eq!(
        round_trip(r#"f(a: 1, b: -2.5e3, c: "x\"y", d: [1 2], e: {k: true}, g: null)"#),
        r#"f(a: 1, b: -2.5e3, c: "x\"y", d: [1, 2], e: {k: true}, g: null)"#
    );
}

#[test]
fn rejects_malformed_maps() {
    for input in [
        "",
        "id.",
        "{ }",
        "{ id",
        "parts[id",
        "parts[id, name",
        "a b",
        "width(unit: $unit)",
        "width()",
        "mediaById<Book>",
        "mediaById<Book>.{ isbn }",
        "<Book>",
        "id |",
        "1abc",
        // GraphQL lexical rules for argument values.
        "f(a: 007)",
        "f(a: -01)",
        "f(a: 1b: 2)",
        "f(a: 1.2.3)",
        r#"f(a: "\u+041")"#,
        "a\u{a0}b",
        r#"f(a: """unterminated)"#,
    ] {
        assert!(parse(input).is_err(), "{input:?} should not parse");
    }
}

#[test]
fn keeps_the_text_of_number_literals() {
    // Neither narrowed to `i32` nor widened to `f64`, which would print `12345678901.0` or turn
    // `1e400` into `inf`.
    assert_eq!(round_trip("f(id: 12345678901)"), "f(id: 12345678901)");
    assert_eq!(round_trip("f(x: 1e400)"), "f(x: 1e400)");
    assert_eq!(round_trip("f(x: -0)"), "f(x: -0)");
}

#[test]
fn decodes_unicode_escapes() {
    assert_eq!(round_trip(r#"f(a: "\u{1F600}")"#), "f(a: \"\u{1F600}\")");
    assert_eq!(round_trip(r#"f(a: "\uD83D\uDE00")"#), "f(a: \"\u{1F600}\")");
    assert!(parse(r#"f(a: "\uD83D")"#).is_err());
    assert!(parse(r#"f(a: "\uDE00\uD83D")"#).is_err());
}

#[test]
fn parses_block_strings() {
    assert_eq!(round_trip(r#"f(a: """x\"""y""")"#), r#"f(a: "x\"\"\"y")"#);
    assert_eq!(
        round_trip("f(a: \"\"\"\n    hello\n      world\n  \"\"\")"),
        r#"f(a: "hello\n  world")"#
    );
}

#[test]
fn rejects_deep_nesting() {
    let depth = 10_000;
    for input in [
        format!("{}x{}", "{ a: ".repeat(depth), " }".repeat(depth)),
        format!("a{}b{}", "[".repeat(depth), "]".repeat(depth)),
        format!("f(a: {}1{})", "[".repeat(depth), "]".repeat(depth)),
    ] {
        let error = parse(&input).expect_err("too deep");
        assert!(error.to_string().contains("nested more than"), "{error}");
    }
    // Realistic nesting is fine.
    let depth = 32;
    parse(&format!("{}x{}", "{ a: ".repeat(depth), " }".repeat(depth))).expect("parses");
}

const SCHEMA: &str = r#"
type Query {
  mediaById(mediaId: ID!): Media
  storeById(id: ID!): Store
  featured: Media
}
type Store { id: ID! city: String! media: [Media!]! parts: [[Part!]]! }
type Part { id: ID! name: String! }
interface Media { id: ID! }
type Book implements Media { id: ID! title: String! isbn: String! author: Author! }
type Movie implements Media { id: ID! movieTitle: String! releaseDate: String! }
type Author { id: ID! books: [Book!]! }
type Product {
  id: ID!
  width(unit: Unit!): Float!
  weight(unit: Unit = METRIC): Float
  dimension: Dimension
  tags: [String!]
  byId(id: ID!): Int
  count(n: Int!): Int
  scale(f: Float!): Int
  find(input: FindMediaInput!): Int
  dims(d: DimensionInput!): Int
}
type Dimension { width: Int! height: Int! }
enum Unit { METRIC IMPERIAL }
input DimensionInput { width: Int! height: Int! }
input WInput { w: Int! h: Int }
input FindMediaInput @oneOf { bookId: ID movieId: ID }
input PartInput { id: ID! name: String! }
"#;

fn schema() -> Schema {
    Schema::parse(SCHEMA, "schema.graphql").expect("test schema should parse")
}

#[track_caller]
fn errors(map: &str, root: &str, expected: &str) -> Vec<String> {
    let schema = schema();
    let expected = apollo_compiler::ast::Type::Named(
        apollo_compiler::Name::new(expected).expect("expected type should be a valid name"),
    );
    let expected = match expected {
        apollo_compiler::ast::Type::Named(ref n) if n.ends_with("__List") => unreachable!(),
        other => other,
    };
    errors_with_type(&schema, map, root, expected)
}

fn errors_with_type(
    schema: &Schema,
    map: &str,
    root: &str,
    expected: apollo_compiler::ast::Type,
) -> Vec<String> {
    let parsed: SelectedValue = parse(map).expect("map should parse");
    validate(
        &parsed,
        &SingleSchemaOutput(schema),
        &apollo_compiler::Name::new(root).expect("root type should be a valid name"),
        schema,
        &expected,
    )
    .into_iter()
    .map(|e| e.message)
    .collect()
}

#[test]
fn validates_paths() {
    assert!(errors("id", "Book", "ID").is_empty());
    assert!(errors("author.id", "Book", "ID").is_empty());
    assert!(errors("<Book>.isbn", "Media", "String").is_empty());
    assert!(errors("featured<Book>.isbn", "Query", "String").is_empty());

    let e = errors("movieId", "Book", "ID");
    assert!(
        e[0].contains("\"movieId\" is not defined on type \"Book\""),
        "{e:?}"
    );
    let e = errors("<Book>.movieTitle", "Media", "String");
    assert!(e[0].contains("not defined on type \"Book\""), "{e:?}");
    let e = errors("author", "Book", "ID");
    assert!(e[0].contains("must end at a scalar or enum"), "{e:?}");
    let e = errors("title.something", "Book", "String");
    assert!(e[0].contains("leaf type"), "{e:?}");
    let e = errors("<Author>.id", "Media", "ID");
    assert!(e[0].contains("can never apply"), "{e:?}");
    let e = errors("isbn", "Book", "ID");
    assert!(e[0].contains("not coercible"), "{e:?}");
}

#[test]
fn validates_arguments() {
    assert!(errors("width(unit: IMPERIAL)", "Product", "Float").is_empty());
    assert!(errors("weight", "Product", "Float").is_empty());
    let e = errors("width", "Product", "Float");
    assert!(e[0].contains("required argument"), "{e:?}");
    let e = errors("width(scale: IMPERIAL)", "Product", "Float");
    assert!(e.iter().any(|m| m.contains("not defined on")), "{e:?}");
    let e = errors("width(unit: FEET)", "Product", "Float");
    assert!(e[0].contains("not coercible"), "{e:?}");
}

#[test]
fn coerces_argument_literals() {
    for map in [
        "byId(id: 12345678901)",
        r#"byId(id: "x")"#,
        "count(n: 2147483647)",
        "scale(f: 1)",
        "scale(f: 1.5e300)",
        r#"find(input: { bookId: "1" })"#,
    ] {
        let e = errors(map, "Product", "Int");
        assert!(e.is_empty(), "{map}: {e:?}");
    }
    for map in [
        "count(n: 2147483648)",
        "scale(f: 1e400)",
        r#"find(input: { bookId: "1", movieId: "2" })"#,
        "find(input: { bookId: null })",
        "find(input: {})",
        "dims(d: { width: 1, height: 2, width: 3 })",
    ] {
        let e = errors(map, "Product", "Int");
        assert!(
            e.iter().any(|m| m.contains("not coercible")),
            "{map}: {e:?}"
        );
    }
}

#[test]
fn validates_objects() {
    assert!(
        errors(
            "{ width: dimension.width, height: dimension.height }",
            "Product",
            "DimensionInput"
        )
        .is_empty()
    );
    assert!(errors("dimension.{ width, height }", "Product", "DimensionInput").is_empty());
    assert!(errors("{ w: dimension.width }", "Product", "WInput").is_empty());

    let e = errors("{ width: dimension.width }", "Product", "DimensionInput");
    assert!(
        e[0].contains("required input field \"DimensionInput.height\""),
        "{e:?}"
    );
    let e = errors(
        "dimension.{ width, width, height }",
        "Product",
        "DimensionInput",
    );
    assert!(e[0].contains("more than once"), "{e:?}");
    let e = errors(
        "dimension.{ width, height, depth }",
        "Product",
        "DimensionInput",
    );
    assert!(
        e[0].contains("\"depth\" is not defined on input type"),
        "{e:?}"
    );
    // A path to an object must be mapped explicitly.
    let e = errors("dimension", "Product", "DimensionInput");
    assert!(e[0].contains("must end at a scalar or enum"), "{e:?}");
}

#[test]
fn validates_one_of_alternatives() {
    assert!(
        errors(
            "{ bookId: <Book>.id } | { movieId: <Movie>.id }",
            "Media",
            "FindMediaInput"
        )
        .is_empty()
    );
    let e = errors(
        "{ bookId: <Book>.id, movieId: <Movie>.id }",
        "Media",
        "FindMediaInput",
    );
    assert!(e.iter().any(|m| m.contains("exactly one field")), "{e:?}");
}

#[test]
fn validates_lists() {
    let schema = schema();
    let ty =
        |s: &str| apollo_compiler::ast::Type::parse(s, "t.graphql").expect("type should parse");
    assert!(errors_with_type(&schema, "media[id]", "Store", ty("[ID!]!")).is_empty());
    assert!(
        errors_with_type(
            &schema,
            "parts[[{ id, name }]]",
            "Store",
            ty("[[PartInput!]]!")
        )
        .is_empty()
    );
    let e = errors_with_type(&schema, "media.id", "Store", ty("[ID!]!"));
    assert!(e[0].contains("cannot traverse a list"), "{e:?}");
    let e = errors_with_type(
        &schema,
        "parts[{ id, name }]",
        "Store",
        ty("[[PartInput!]]!"),
    );
    assert!(e[0].contains("nested list"), "{e:?}");
    let e = errors_with_type(&schema, "id[id]", "Store", ty("[ID!]!"));
    assert!(e[0].contains("non-list"), "{e:?}");
}

#[test]
fn detects_arguments() {
    assert!(!has_arguments(
        &parse("{ id } | address.id").expect("map should parse")
    ));
    assert!(has_arguments(
        &parse("width(unit: IMPERIAL)").expect("map should parse")
    ));
    assert!(has_arguments(
        &parse("{ w: width(unit: IMPERIAL) }").expect("map should parse")
    ));
    assert!(has_arguments(
        &parse("parts[{ id(x: 1) }]").expect("map should parse")
    ));
}

#[test]
fn checks_mappability_per_alternative() {
    let schema = schema();
    let mappable = |map: &str, concrete| {
        is_mappable(&parse(map).expect("map should parse"), &schema, &concrete)
    };
    // A root type condition picks the alternative for each concrete type.
    assert!(mappable(
        "{ bookId: <Book>.isbn } | { movieId: <Movie>.id }",
        name!("Book")
    ));
    assert!(mappable(
        "{ bookId: <Book>.isbn } | { movieId: <Movie>.id }",
        name!("Movie")
    ));
    // Without type conditions, one fully defined alternative is enough.
    assert!(mappable(
        "{ id } | { addressId: author.id } | { title }",
        name!("Book")
    ));
    assert!(!mappable(
        "{ nope } | { addressId: author.nope }",
        name!("Book")
    ));
    // A type condition after a field switches to that type.
    assert!(mappable("mediaById<Book>.isbn", name!("Query")));
    assert!(!mappable("mediaById<Book>.nope", name!("Query")));
}

#[test]
fn checks_mappability() {
    let schema = schema();
    let map = parse("{ bookId: <Book>.isbn } | { movieId: <Movie>.id }").expect("map should parse");
    assert!(is_mappable(&map, &schema, &name!("Book")));
    assert!(is_mappable(&map, &schema, &name!("Movie")));
    let map = parse("{ bookId: <Book>.isbn }").expect("map should parse");
    assert!(!is_mappable(&map, &schema, &name!("Movie")));
    let map = SelectedValue::field(name!("isbn"));
    assert!(is_mappable(&map, &schema, &name!("Book")));
    assert!(!is_mappable(&map, &schema, &name!("Movie")));
}

#[test]
fn checks_mappability_of_nested_fields() {
    let schema = schema();
    let mappable = |map: &str, concrete| {
        is_mappable(&parse(map).expect("map should parse"), &schema, &concrete)
    };
    // Every level of a path must be defined, not just the root field.
    assert!(mappable("author.id", name!("Book")));
    assert!(!mappable("author.name", name!("Book")));
    assert!(mappable("dimension.{ width, height }", name!("Product")));
    assert!(!mappable("dimension.{ width, depth }", name!("Product")));
    assert!(!mappable("{ w: dimension.depth }", name!("Product")));
    // Nested type conditions are checked against their type.
    assert!(mappable("featured<Book>.isbn", name!("Query")));
    assert!(!mappable("featured<Movie>.isbn", name!("Query")));
}
