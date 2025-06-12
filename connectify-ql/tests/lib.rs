use connectify_ql::aol_parse;
use insta::assert_snapshot;

#[test]
fn parse_aol_is_correct_with_no_warnings() {
    let aol = "use check.CheckService::Check as Mutation::check;
use check.CheckService::time_since as Query::timeSince;";

    let parsed = aol_parse(aol).unwrap();
    assert_eq!(parsed.warnings, None);
    assert_snapshot!(parsed.cst);

    let map =
        connectify_ql::linking::grpc::service_mapping::parse_service_mapping(&parsed.cst, aol)
            .unwrap();
    assert_snapshot!(format!("{map:#?}"));
}
