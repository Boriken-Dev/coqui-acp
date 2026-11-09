//! Numbers in the shared vectors: how each token is read and spelled, and the ones that are
//! refused.

mod support;

use coqui_acp_history::{Body, Document};
use support::{ID, vectors};

fn line_with(number: &str) -> String {
    format!(
        r#"{{"method":"session/update","params":{{"update":{{"rawOutput":{number},"sessionUpdate":"x"}}}}}}"#
    )
}

fn file(number: &str) -> String {
    format!(
        "{{\"format\":\"coqui-acp-history\",\"version\":1,\"id\":\"{ID}\",\"origin\":{{\"harness\":\"claude\"}}}}\n{}\n",
        line_with(number)
    )
}

#[test]
fn every_number_vector_is_read_and_spelled_as_it_says() {
    let all = vectors("numbers.json");
    let cases = all["cases"].as_array().unwrap();
    assert!(cases.len() >= 40);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let number = case["number"].as_str().unwrap();
        let text = file(number);
        if let Some(code) = case.get("refused").and_then(|c| c.as_str()) {
            let error = text.parse::<Document>().expect_err(name);
            assert_eq!(error.code(), code, "{name}: {number}");
            assert_eq!(error.line(), Some(2), "{name}");
            continue;
        }
        let doc: Document = text
            .parse()
            .unwrap_or_else(|e| panic!("{name}: {number}: {e}"));
        let written = doc.to_string().unwrap();
        let want = file(case["canonical"].as_str().unwrap());
        assert_eq!(written, want, "{name}: {number}");
        // The spelling is a fixed point: reading it and writing it again changes nothing.
        let again: Document = written.parse().unwrap();
        assert_eq!(again, doc, "{name}");
        assert_eq!(again.to_string().unwrap(), written, "{name}");
    }
}

#[test]
fn a_double_spells_the_same_whatever_it_was_read_from() {
    let doc: Document = file("[1e2,100.0,1E+2,100e0]").parse().unwrap();
    let Body::Update { update } = &doc.lines[0].body else {
        panic!()
    };
    let spelled = update["rawOutput"].as_array().unwrap();
    assert!(spelled.iter().all(|n| n.as_f64() == Some(100.0)));
    assert!(
        doc.to_string()
            .unwrap()
            .contains("[100.0,100.0,100.0,100.0]")
    );
}

#[test]
fn an_integer_stays_exact_and_a_double_stays_a_double() {
    let doc: Document = file("[18446744073709551615,-9223372036854775808,9007199254740993]")
        .parse()
        .unwrap();
    assert!(
        doc.to_string()
            .unwrap()
            .contains("[18446744073709551615,-9223372036854775808,9007199254740993]")
    );
    let doc: Document = file("[1.0]").parse().unwrap();
    assert!(doc.to_string().unwrap().contains("[1.0]"));
}
