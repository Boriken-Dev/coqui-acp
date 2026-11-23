//! The forms a login asks the person, as the crate derives them, read by the secret form a frontend draws them from.

use coqui_acp::FieldKind;
use coqui_acp::secret::{Error, Form, Limits};
use coqui_acp_api::LoginForm;
use coqui_acp_api::guatiao::schema::{ArmBuilder, FieldBuilder, KindBuilder, SchemaBuilder};
use coqui_acp_api::guatiao::{FromValue, Schema, ToValue};
use coqui_acp_harness::provider::{schema_of, to_json};
use serde_json::{Value, json};

#[derive(ToValue, FromValue, Schema, Debug)]
#[map(crate = ::coqui_acp_api::guatiao, deny_unknown_fields)]
struct Settings {
    #[schema(title = "Address", format = "uri", section = "s", order = 1, advanced)]
    address: Option<String>,
    #[schema(title = "Port", default = 8080, order = 2)]
    port: u16,
    #[schema(sensitive)]
    key: String,
    verbose: bool,
    ratio: f64,
    mode: Mode,
}

#[derive(ToValue, FromValue, Schema, Debug)]
#[map(crate = ::coqui_acp_api::guatiao)]
enum Mode {
    Fast,
    Slow,
}

#[derive(ToValue, FromValue, Schema, Debug)]
#[map(crate = ::coqui_acp_api::guatiao)]
struct Wide {
    big: u64,
}

fn parse(value: &coqui_acp_api::guatiao::Value) -> Result<Form, Error> {
    Form::parse(&to_json(value), &Limits::new())
}

fn secret(form: &Form) -> Vec<&str> {
    form.fields
        .iter()
        .filter(|field| field.secret)
        .map(|field| field.field.name.as_str())
        .collect()
}

#[test]
fn the_secret_form_every_login_asks_for_is_a_form() {
    let login = LoginForm::secret("Enter the key", "api_key");
    let json = to_json(&login.schema);
    assert_eq!(json["additionalProperties"], false, "{json}");
    assert!(json["$schema"].is_string(), "{json}");
    let form = parse(&login.schema).unwrap();
    assert_eq!(secret(&form), ["api_key"]);
    assert!(form.fields[0].field.required);
}

#[test]
fn the_flag_a_login_waits_on_is_a_form() {
    let schema = SchemaBuilder::new()
        .field(
            FieldBuilder::new("started", KindBuilder::bool())
                .title("Antigravity is started and signed in")
                .description("Start Antigravity, sign in there, then continue."),
        )
        .finish()
        .unwrap();
    let form = parse(&schema).unwrap();
    assert!(secret(&form).is_empty());
    assert!(matches!(
        form.fields[0].field.kind,
        FieldKind::Boolean { .. }
    ));
}

#[test]
fn a_derived_settings_schema_is_a_form_with_its_choices_labelled() {
    let form = parse(&schema_of::<Settings>()).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(secret(&form), ["key"]);
    let kind = |name: &str| &form.field(name).unwrap().field.kind;
    assert!(
        matches!(kind("address"), FieldKind::Text { format: Some(format), .. } if format == "uri")
    );
    assert!(matches!(
        kind("port"),
        FieldKind::Integer {
            minimum: Some(0),
            maximum: Some(65535),
            default: Some(8080),
        }
    ));
    let FieldKind::Choice { options, .. } = kind("mode") else {
        panic!("{:?}", kind("mode"));
    };
    assert_eq!(options.len(), 2);
}

#[test]
fn a_nullable_field_is_the_field_of_its_type_and_may_be_left_out() {
    let schema = SchemaBuilder::new()
        .field(FieldBuilder::new("note", KindBuilder::string().nullable()))
        .field(FieldBuilder::new("pin", KindBuilder::string().nullable()).sensitive())
        .finish()
        .unwrap();
    assert_eq!(
        to_json(&schema)["properties"]["note"]["type"],
        json!(["string", "null"])
    );
    let form = parse(&schema).unwrap();
    assert!(matches!(form.fields[0].field.kind, FieldKind::Text { .. }));
    assert_eq!(secret(&form), ["pin"]);
    assert!(!form.fields[1].field.required);
}

#[test]
fn a_choice_made_of_an_enumeration_keeps_its_labels() {
    let schema = SchemaBuilder::new()
        .field(FieldBuilder::new(
            "plan",
            KindBuilder::enumeration(&[("a", "Plan A"), ("b", "Plan B")]),
        ))
        .finish()
        .unwrap();
    let form = parse(&schema).unwrap();
    let FieldKind::Choice { options, .. } = &form.fields[0].field.kind else {
        panic!("{:?}", form.fields[0].field.kind);
    };
    assert_eq!(options[0].value, "a");
    assert_eq!(options[0].title, "Plan A");
    assert_eq!(options[1].title, "Plan B");
}

#[test]
fn what_a_flat_form_cannot_draw_is_refused_naming_the_keyword() {
    let said = |schema: &coqui_acp_api::guatiao::Value| parse(schema).unwrap_err().to_string();

    // A fraction or a non-number as a whole-number bound.
    for bound in [json!(1.5), json!("9")] {
        let schema = json!({"properties": {"n": {"type": "integer", "maximum": bound}}});
        let said = Form::parse(&schema, &Limits::new())
            .unwrap_err()
            .to_string();
        assert!(said.contains("/properties/n/maximum"), "{said}");
    }

    // Bytes are no control's kind.
    let bytes = SchemaBuilder::new()
        .field(FieldBuilder::new("blob", KindBuilder::bytes()))
        .finish()
        .unwrap();
    assert!(
        said(&bytes).contains("/properties/blob/type"),
        "{}",
        said(&bytes)
    );

    // A secret that carries a default or a list of values.
    let kept = SchemaBuilder::new()
        .field(
            FieldBuilder::new("key", KindBuilder::string())
                .sensitive()
                .default("hunter2"),
        )
        .finish()
        .unwrap();
    assert!(
        said(&kept).contains("/properties/key/default"),
        "{}",
        said(&kept)
    );

    // A union of shapes.
    let union = SchemaBuilder::new()
        .field(FieldBuilder::new(
            "either",
            KindBuilder::variant("kind", vec![ArmBuilder::new("a", "A")]),
        ))
        .finish()
        .unwrap();
    assert!(parse(&union).is_err());
}

#[test]
fn a_root_that_would_let_a_frontend_draw_more_than_the_form_declares_is_refused() {
    let derived = to_json(&LoginForm::secret("Key", "api_key").schema);
    let with = |key: &str, value: Value| {
        let mut schema = derived.clone();
        schema[key] = value;
        Form::parse(&schema, &Limits::new())
            .unwrap_err()
            .to_string()
    };
    for open in [json!(true), json!({"type": "string"})] {
        let said = with("additionalProperties", open);
        assert!(said.contains("/additionalProperties"), "{said}");
    }
    let said = with("$schema", json!("http://json-schema.org/draft-04/schema#"));
    assert!(said.contains("/$schema"), "{said}");
    let said = with("$defs", json!({}));
    assert!(said.contains("/$defs"), "{said}");
    let said = with("unevaluatedProperties", json!(true));
    assert!(said.contains("/unevaluatedProperties"), "{said}");
}

#[test]
fn the_bounds_of_a_wide_integer_limit_nothing() {
    let form = parse(&schema_of::<Wide>()).unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(
        form.fields[0].field.kind,
        FieldKind::Integer { maximum: None, .. }
    ));
}
