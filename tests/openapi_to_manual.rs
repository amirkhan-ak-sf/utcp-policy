//! Conformance tests: OpenAPI fixture -> UTCP Manual.
//!
//! These run the same converter the policy uses at startup against a
//! representative subset of OpenAPI features (path/query/header/body
//! parameters, security schemes, $refs) and assert the produced Manual
//! has the expected tools, URLs, and inputs.

use serde_json::Value;
use utcp_manual_validator::manual::{
    openapi::{convert, ConvertOptions},
    CallTemplate,
};

const PETSTORE: &str = include_str!("petstore.json");

fn opts() -> ConvertOptions<'static> {
    ConvertOptions {
        utcp_version: "1.0.1",
        tool_name_prefix: "",
    }
}

#[test]
fn petstore_produces_expected_tools() {
    let spec: Value = serde_json::from_str(PETSTORE).unwrap();
    let manual = convert(&spec, &opts()).expect("conversion");
    let names: Vec<&str> = manual.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"getPet"));
    assert!(names.contains(&"listPets"));
    assert!(names.contains(&"createPet"));
}

#[test]
fn petstore_path_param_surfaces_in_url_and_inputs() {
    let spec: Value = serde_json::from_str(PETSTORE).unwrap();
    let manual = convert(&spec, &opts()).unwrap();
    let get_pet = manual.tools.iter().find(|t| t.name == "getPet").unwrap();
    match &get_pet.tool_call_template {
        CallTemplate::Http(http) => {
            assert!(http.url.ends_with("/pets/{petId}"));
            assert_eq!(http.http_method, "GET");
        }
    }
    let inputs = get_pet.inputs.as_ref().unwrap();
    let req = inputs.get("required").and_then(Value::as_array).unwrap();
    assert!(req.iter().any(|v| v == "petId"));
}

#[test]
fn petstore_post_has_body_field() {
    let spec: Value = serde_json::from_str(PETSTORE).unwrap();
    let manual = convert(&spec, &opts()).unwrap();
    let create = manual
        .tools
        .iter()
        .find(|t| t.name == "createPet")
        .unwrap();
    match &create.tool_call_template {
        CallTemplate::Http(http) => {
            assert_eq!(http.body_field.as_deref(), Some("body"));
            assert_eq!(http.http_method, "POST");
        }
    }
}
