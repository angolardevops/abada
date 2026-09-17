//! Both candidates against grpc-gateway's JSON vectors. The findings are the
//! input of ADR 0001; this test keeps them true.

#[test]
fn findings_match_the_recorded_ones() {
    let expected = include_str!("../expected.txt");
    let got = abada_json_bench::summary(&abada_json_bench::compare());
    let changed: Vec<String> = expected
        .lines()
        .zip(got.lines())
        .filter(|(e, g)| e != g)
        .map(|(e, g)| format!("- {e}\n+ {g}"))
        .collect();
    assert!(
        changed.is_empty() && expected.lines().count() == got.lines().count(),
        "findings changed (cargo run -p abada-json-bench --bin correctness -- --bless, then revisit ADR 0001):\n{}",
        changed.join("\n")
    );
}

/// The contract's highest-stakes shapes, stated directly rather than through
/// the summary: prost-reflect writes `Any` and `FieldMask` as grpc-gateway does.
#[test]
fn prost_reflect_writes_any_and_field_mask_like_grpc_gateway() {
    for f in abada_json_bench::compare() {
        if f.candidate == "prost-reflect"
            && matches!(
                f.case.as_str(),
                "operation_with_any_result"
                    | "operation_failed_with_wkt_any"
                    | "update_container_request_field_mask"
            )
        {
            assert_eq!(f.decode, abada_json_bench::Verdict::Identical, "{}", f.case);
            assert_eq!(
                f.encode_omit,
                abada_json_bench::Verdict::Identical,
                "{}",
                f.case
            );
        }
    }
}

/// abada's codec answers exactly like grpc-gateway on every body: the same
/// accept/reject and decoded message, and the same bytes out.
#[test]
fn abada_is_grpc_gateway_on_every_body() {
    use abada_json_bench::Verdict;
    let mut seen = 0;
    for f in abada_json_bench::compare() {
        if f.candidate != "abada" {
            continue;
        }
        seen += 1;
        assert!(
            matches!(f.decode, Verdict::Identical | Verdict::BothReject),
            "{}: decode {:?}",
            f.case,
            f.decode
        );
        for e in [&f.encode_emit, &f.encode_omit] {
            assert!(
                matches!(e, Verdict::SameBytes | Verdict::BothReject),
                "{}: encode {e:?}",
                f.case
            );
        }
    }
    assert_eq!(seen, 28);
}
