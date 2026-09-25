use apollo_compiler::Schema;
use apollo_compiler::name;

use super::SelectedValue;
use super::parse;
use super::validate::SingleSchemaOutput;
use super::validate::has_arguments;
use super::validate::validate;
use super::value::is_mappable;
use super::value::selections_for_type;

#[track_caller]
fn round_trip(input: &str) -> String {
    let parsed = parse(input).unwrap_or_else(|e| panic!("failed to parse {input:?}: {e}"));
    let printed = parsed.to_string();
    // Printing is canonical: reparsing the printed form yields the same map.
    assert_eq!(parse(&printed).unwrap(), parsed, "reparse of {printed:?}");
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
        r#"f(a: 1, b: -2500.0, c: "x\"y", d: [1, 2], e: {k: true}, g: null)"#
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
    ] {
        assert!(parse(input).is_err(), "{input:?} should not parse");
    }
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
}
type Dimension { width: Int! height: Int! }
enum Unit { METRIC IMPERIAL }
input DimensionInput { width: Int! height: Int! }
input WInput { w: Int! h: Int }
input FindMediaInput @oneOf { bookId: ID movieId: ID }
input PartInput { id: ID! name: String! }
"#;

fn schema() -> Schema {
    Schema::parse(SCHEMA, "schema.graphql").unwrap()
}

#[track_caller]
fn errors(map: &str, root: &str, expected: &str) -> Vec<String> {
    let schema = schema();
    let expected = apollo_compiler::ast::Type::Named(apollo_compiler::Name::new(expected).unwrap());
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
    let parsed: SelectedValue = parse(map).unwrap();
    validate(
        &parsed,
        &SingleSchemaOutput(schema),
        &apollo_compiler::Name::new(root).unwrap(),
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
    let ty = |s: &str| apollo_compiler::ast::Type::parse(s, "t.graphql").unwrap();
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
    assert!(!has_arguments(&parse("{ id } | address.id").unwrap()));
    assert!(has_arguments(&parse("width(unit: IMPERIAL)").unwrap()));
    assert!(has_arguments(
        &parse("{ w: width(unit: IMPERIAL) }").unwrap()
    ));
    assert!(has_arguments(&parse("parts[{ id(x: 1) }]").unwrap()));
}

#[test]
fn computes_key_selections_per_type() {
    let schema = schema();
    let map = parse("{ bookId: <Book>.isbn } | { movieId: <Movie>.id }").unwrap();
    let book: Vec<String> = selections_for_type(&map, &schema, &name!("Book"))
        .iter()
        .map(|(_, t)| t.to_string())
        .collect();
    assert_eq!(book, ["isbn"]);
    let movie: Vec<String> = selections_for_type(&map, &schema, &name!("Movie"))
        .iter()
        .map(|(_, t)| t.to_string())
        .collect();
    assert_eq!(movie, ["id"]);

    let map = parse("{ id } | { addressId: author.id } | { title }").unwrap();
    let book: Vec<String> = selections_for_type(&map, &schema, &name!("Book"))
        .iter()
        .map(|(_, t)| t.to_string())
        .collect();
    assert_eq!(book, ["id", "author { id }", "title"]);

    let map = parse("dimension.{ width, height }").unwrap();
    assert_eq!(
        selections_for_type(&map, &schema, &name!("Product"))[0]
            .1
            .to_string(),
        "dimension { width height }"
    );
    let map = parse("mediaById<Book>.isbn").unwrap();
    assert_eq!(
        selections_for_type(&map, &schema, &name!("Query"))[0]
            .1
            .to_string(),
        "mediaById { ... on Book { isbn } }"
    );
}

#[test]
fn checks_mappability() {
    let schema = schema();
    let map = parse("{ bookId: <Book>.isbn } | { movieId: <Movie>.id }").unwrap();
    assert!(is_mappable(&map, &schema, &name!("Book")));
    assert!(is_mappable(&map, &schema, &name!("Movie")));
    let map = parse("{ bookId: <Book>.isbn }").unwrap();
    assert!(!is_mappable(&map, &schema, &name!("Movie")));
    let map = SelectedValue::field(name!("isbn"));
    assert!(is_mappable(&map, &schema, &name!("Book")));
    assert!(!is_mappable(&map, &schema, &name!("Movie")));
}
