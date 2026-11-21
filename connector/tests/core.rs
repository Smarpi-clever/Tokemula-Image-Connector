use serde_json::{Value, json};
use tokemula_image_connector::{
    Catalog, ErrorCode, ExecutionMode, FeeReceipt, FeeStatus, Operation, Pricing, SelectionRequest,
    Visibility,
};

const CATALOG: &str = include_str!("fixtures/catalog.json");

fn fixture() -> Value {
    serde_json::from_str(CATALOG).unwrap()
}

fn parse(value: &Value) -> Catalog {
    Catalog::from_json(&value.to_string()).unwrap()
}

fn generation() -> SelectionRequest<'static> {
    SelectionRequest::new(Operation::Generation, ExecutionMode::Sync)
}

#[test]
fn explicit_model_is_preserved_even_when_another_family_is_preferred() {
    let catalog = Catalog::from_json(CATALOG).unwrap();
    let mut request = generation();
    request.model_id = Some("sample-fast");
    let selected = catalog.select(&request).unwrap();
    assert_eq!(selected.id, "sample-fast");
    assert_eq!(selected.pricing, Pricing::Unknown {});

    request.model_id = Some("SAMPLE-FAST");
    let error = catalog.select(&request).unwrap_err();
    assert_eq!(error.code, ErrorCode::ModelNotFound);
    assert_eq!(error.alternatives, ["sample-fast", "sample-26"]);
}

#[test]
fn explicit_rejections_explain_the_actual_mismatch_and_only_offer_compatible_models() {
    let catalog = Catalog::from_json(CATALOG).unwrap();
    let cases = [
        (
            "sample-paused",
            Operation::Generation,
            ExecutionMode::Sync,
            None,
            None,
            1,
            ErrorCode::ModelUnavailable,
            vec!["sample-fast", "sample-26"],
        ),
        (
            "sample-fast",
            Operation::Edit,
            ExecutionMode::SingleAsync,
            None,
            None,
            1,
            ErrorCode::UnsupportedOperation,
            vec!["sample-26"],
        ),
        (
            "sample-26",
            Operation::Edit,
            ExecutionMode::Sync,
            None,
            None,
            1,
            ErrorCode::UnsupportedMode,
            vec![],
        ),
        (
            "sample-fast",
            Operation::Generation,
            ExecutionMode::Sync,
            Some("1536x1024"),
            None,
            1,
            ErrorCode::UnsupportedParameter,
            vec!["sample-26"],
        ),
        (
            "sample-fast",
            Operation::Generation,
            ExecutionMode::Sync,
            None,
            Some("high"),
            1,
            ErrorCode::UnsupportedParameter,
            vec!["sample-26"],
        ),
        (
            "sample-fast",
            Operation::Generation,
            ExecutionMode::Sync,
            None,
            None,
            3,
            ErrorCode::UnsupportedParameter,
            vec!["sample-26"],
        ),
        (
            "sample-fast",
            Operation::Generation,
            ExecutionMode::Sync,
            None,
            None,
            0,
            ErrorCode::UnsupportedParameter,
            vec![],
        ),
    ];
    for (model_id, operation, mode, size, quality, image_count, code, alternatives) in cases {
        let error = catalog
            .select(&SelectionRequest {
                model_id: Some(model_id),
                operation,
                mode,
                size,
                quality,
                image_count,
            })
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.alternatives, alternatives);
        assert!(!error.message.is_empty());
    }
}

#[test]
fn automatic_choice_applies_family_then_rank_and_preserves_ties() {
    let mut value = fixture();
    assert_eq!(parse(&value).select(&generation()).unwrap().id, "sample-26");
    let mut second_family = value["data"][1].clone();
    second_family["id"] = json!("sample-26-second");
    second_family["preference_rank"] = json!(2);
    value["data"].as_array_mut().unwrap().push(second_family);
    assert_eq!(
        parse(&value).select(&generation()).unwrap().id,
        "sample-26-second"
    );
    value["data"][1]["preference_rank"] = json!(2);
    assert_eq!(parse(&value).select(&generation()).unwrap().id, "sample-26");
}

#[test]
fn explicit_max_rank_sorts_before_missing_rank_in_the_same_family() {
    let mut value = fixture();
    value["data"][1]
        .as_object_mut()
        .unwrap()
        .remove("preference_rank");
    let mut ranked = value["data"][1].clone();
    ranked["id"] = json!("sample-explicit-max");
    ranked["preference_rank"] = json!(u32::MAX);
    value["data"].as_array_mut().unwrap().push(ranked);
    assert_eq!(
        parse(&value).select(&generation()).unwrap().id,
        "sample-explicit-max"
    );
}

#[test]
fn names_are_never_used_as_family_markers_and_missing_rank_is_last() {
    let mut value = fixture();
    value["data"][0]["id"] = json!("image-2.6-impostor");
    value["data"][1].as_object_mut().unwrap().remove("family");
    value["data"][0]["preference_rank"] = json!(100);
    assert_eq!(parse(&value).select(&generation()).unwrap().id, "sample-26");
    value["data"][0]["preference_rank"] = json!(0);
    assert_eq!(
        parse(&value).select(&generation()).unwrap().id,
        "image-2.6-impostor"
    );
    value["data"][0]
        .as_object_mut()
        .unwrap()
        .remove("preference_rank");
    assert_eq!(parse(&value).select(&generation()).unwrap().id, "sample-26");
}

#[test]
fn automatic_choice_filters_unavailable_and_incompatible_models_before_ranking() {
    let mut value = fixture();
    value["data"][1]["capabilities"] = json!([]);
    assert_eq!(
        parse(&value).select(&generation()).unwrap().id,
        "sample-fast"
    );
    let mut request = generation();
    request.size = Some("unsupported-size");
    assert_eq!(
        parse(&value).select(&request).unwrap_err().code,
        ErrorCode::NoCompatibleModel
    );
    let edit = SelectionRequest::new(Operation::Edit, ExecutionMode::SingleAsync);
    assert_eq!(
        Catalog::from_json(CATALOG)
            .unwrap()
            .select(&edit)
            .unwrap()
            .id,
        "sample-26"
    );
}

#[test]
fn empty_parameter_lists_allow_defaults_and_reject_explicit_values_offline() {
    let mut value = fixture();
    value["data"][0]["capabilities"][0]["parameters"]["sizes"] = json!([]);
    value["data"][0]["capabilities"][0]["parameters"]["qualities"] = json!([]);
    let catalog = parse(&value);
    let mut request = generation();
    request.model_id = Some("sample-fast");
    assert_eq!(catalog.select(&request).unwrap().id, "sample-fast");
    for (size, quality) in [(Some("1024x1024"), None), (None, Some("standard"))] {
        request.size = size;
        request.quality = quality;
        assert_eq!(
            catalog.select(&request).unwrap_err().code,
            ErrorCode::UnsupportedParameter
        );
    }
}

#[test]
fn submission_denial_is_distinct_from_empty_or_unmatched_catalog() {
    let mut value = fixture();
    value["can_submit"] = json!(false);
    value["reason_code"] = json!("insufficient_balance");
    let catalog = parse(&value);
    assert!(!catalog.can_submit());
    let error = catalog.select(&generation()).unwrap_err();
    assert_eq!(error.code, ErrorCode::SubmissionNotAllowed);
    assert!(error.message.contains("余额不足"));
    assert!(error.alternatives.is_empty());
    let empty = Catalog::from_json(r#"{"can_submit":true,"data":[]}"#).unwrap();
    assert!(empty.models().is_empty());
    assert_eq!(
        empty.select(&generation()).unwrap_err().code,
        ErrorCode::NoCompatibleModel
    );
    let batch = SelectionRequest::new(Operation::Generation, ExecutionMode::Batch);
    assert_eq!(
        Catalog::from_json(CATALOG)
            .unwrap()
            .select(&batch)
            .unwrap_err()
            .code,
        ErrorCode::NoCompatibleModel
    );
}

#[test]
fn malformed_and_ambiguous_catalogs_fail_at_the_boundary() {
    for malformed in ["not-json", "{}", r#"{"can_submit":"yes","data":[]}"#] {
        assert_eq!(
            Catalog::from_json(malformed).unwrap_err().code,
            ErrorCode::InvalidCatalog
        );
    }
    let mut bad_catalogs = Vec::new();
    let mut value = fixture();
    value["data"][1]["id"] = value["data"][0]["id"].clone();
    bad_catalogs.push(value);
    let mut value = fixture();
    value["data"][0]["id"] = json!(" ");
    bad_catalogs.push(value);
    let mut value = fixture();
    value["data"][0]["capabilities"][0]["parameters"]["max_images"] = json!(0);
    bad_catalogs.push(value);
    let mut value = fixture();
    let duplicate = value["data"][0]["capabilities"][0].clone();
    value["data"][0]["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    bad_catalogs.push(value);
    let mut value = fixture();
    value["data"][1]["pricing"]["amount"]["ledger_amount"] = json!("-1");
    bad_catalogs.push(value);
    for value in bad_catalogs {
        assert_eq!(
            Catalog::from_json(&value.to_string()).unwrap_err().code,
            ErrorCode::InvalidCatalog
        );
    }
}

#[test]
fn unknown_hidden_and_zero_prices_keep_their_meaning_without_changing_permissions() {
    let mut value = fixture();
    value["data"][2]["availability"] = json!("configured");
    let catalog = parse(&value);
    for (id, status) in [
        ("sample-fast", "unknown"),
        ("sample-paused", "hidden"),
        ("sample-26", "available"),
    ] {
        let mut request = generation();
        request.model_id = Some(id);
        let model = catalog.select(&request).unwrap();
        assert_eq!(
            serde_json::to_value(&model.pricing).unwrap()["status"],
            status
        );
    }
    let Pricing::Available { amount } = &catalog.models()[1].pricing else {
        panic!("expected available quote")
    };
    assert_eq!(amount.ledger_amount, "0");
    assert_eq!(amount.ledger_unit, "TEST_CREDIT");
    assert!(amount.display.is_none());
    for status in ["hidden", "unknown"] {
        let mut invalid = fixture();
        invalid["data"][1]["pricing"]["status"] = json!(status);
        assert_eq!(
            Catalog::from_json(&invalid.to_string()).unwrap_err().code,
            ErrorCode::InvalidCatalog
        );
    }
}

#[test]
fn receipt_status_visibility_and_zero_amount_remain_distinct() {
    let zero = FeeReceipt::from_json(r#"{"status":"settled","visibility":"visible","amount":{"ledger_amount":"0","ledger_unit":"TEST_CREDIT"}}"#).unwrap();
    assert_eq!(zero.status(), FeeStatus::Settled);
    assert_eq!(zero.amount().unwrap().ledger_amount, "0");
    for (status, visibility, expected_status, expected_visibility) in [
        (
            "pending",
            "visible",
            FeeStatus::Pending,
            Visibility::Visible,
        ),
        (
            "unavailable",
            "visible",
            FeeStatus::Unavailable,
            Visibility::Visible,
        ),
        ("settled", "hidden", FeeStatus::Settled, Visibility::Hidden),
        ("pending", "hidden", FeeStatus::Pending, Visibility::Hidden),
    ] {
        let receipt =
            FeeReceipt::from_json(&json!({"status":status,"visibility":visibility}).to_string())
                .unwrap();
        assert_eq!(receipt.status(), expected_status);
        assert_eq!(receipt.visibility(), expected_visibility);
        assert!(receipt.amount().is_none());
    }
}

#[test]
fn receipts_reject_amounts_that_contradict_settlement_or_visibility() {
    for value in [
        json!({"status":"settled","visibility":"visible"}),
        json!({"status":"pending","visibility":"visible","amount":{"ledger_amount":"0","ledger_unit":"TEST_CREDIT"}}),
        json!({"status":"settled","visibility":"hidden","amount":{"ledger_amount":"0","ledger_unit":"TEST_CREDIT"}}),
        json!({"status":"settled","visibility":"visible","amount":{"ledger_amount":"NaN","ledger_unit":"TEST_CREDIT"}}),
    ] {
        assert_eq!(
            FeeReceipt::from_json(&value.to_string()).unwrap_err().code,
            ErrorCode::InvalidFeeReceipt
        );
    }
}

#[test]
fn amounts_preserve_exact_text_and_separate_display_currency() {
    let receipt = FeeReceipt::from_json(r#"{"status":"settled","visibility":"visible","amount":{"ledger_amount":"9007199254740993.00000001","ledger_unit":"TEST_CREDIT","display":{"amount":"3.50","currency":"TEST_DISPLAY"}}}"#).unwrap();
    let amount = receipt.amount().unwrap();
    assert_eq!(amount.ledger_amount, "9007199254740993.00000001");
    assert_eq!(amount.ledger_unit, "TEST_CREDIT");
    assert_eq!(amount.display.as_ref().unwrap().currency, "TEST_DISPLAY");
    assert_eq!(
        serde_json::to_value(&receipt).unwrap()["amount"]["display"]["amount"],
        "3.50"
    );
}

#[test]
fn diagnostics_do_not_echo_raw_payloads_or_untrusted_reason_values() {
    const MARKER: &str = "SYNTHETIC_AUTHORIZATION_DO_NOT_ECHO";
    let malformed = format!(r#"{{"can_submit":"{MARKER}","Authorization":"{MARKER}","data":[]}}"#);
    let catalog_error = Catalog::from_json(&malformed).unwrap_err();
    let fee_error = FeeReceipt::from_json(&format!(r#"{{"status":"{MARKER}"}}"#)).unwrap_err();
    let value = json!({"can_submit":false,"reason_code":MARKER,"data":[]});
    let denied = parse(&value).select(&generation()).unwrap_err();
    let mut request = generation();
    request.model_id = Some(MARKER);
    let missing = Catalog::from_json(CATALOG)
        .unwrap()
        .select(&request)
        .unwrap_err();
    for error in [catalog_error, fee_error, denied, missing] {
        for output in [
            format!("{error}"),
            format!("{error:?}"),
            serde_json::to_string(&error).unwrap(),
        ] {
            assert!(!output.contains(MARKER));
            assert!(!output.contains("Authorization"));
            assert!(!output.contains("can_submit"));
        }
    }
}
