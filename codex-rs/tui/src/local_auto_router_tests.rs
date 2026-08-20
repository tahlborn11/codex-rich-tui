use super::*;
use pretty_assertions::assert_eq;

#[test]
fn bounds_text_at_eight_kib_without_splitting_utf8() {
    let text = "é".repeat(10_000);
    let bounded = bounded_text(&text).expect("text");
    assert!(bounded.len() <= MAX_PROMPT_BYTES);
    assert!(bounded.is_char_boundary(bounded.len()));
}

#[test]
fn unavailable_target_uses_configured_model() {
    let routed = target_or_fallback(
        LUNA,
        ReasoningEffort::Low,
        "Luna (low)",
        &HashSet::new(),
        "gpt-5.4".to_string(),
        Some(ReasoningEffort::Medium),
    );
    assert_eq!(routed.model, "gpt-5.4");
    assert_eq!(routed.effort, Some(ReasoningEffort::Medium));
}

fn config() -> TuiAutoConfig {
    TuiAutoConfig {
        classifier_model: "local".to_string(),
        default_selected: false,
        endpoint: "http://localhost".to_string(),
        timeout_ms: 500,
        low_threshold: 0.4,
        medium_threshold: 0.7,
    }
}

fn all_targets() -> HashSet<String> {
    [LUNA, TERRA, SOL].into_iter().map(str::to_string).collect()
}

#[test]
fn score_boundaries_select_expected_models() {
    assert_eq!(
        route_score(0.39, &config(), &all_targets(), "other".to_string(), None).model,
        LUNA
    );
    assert_eq!(
        route_score(0.4, &config(), &all_targets(), "other".to_string(), None).model,
        TERRA
    );
    assert_eq!(
        route_score(0.7, &config(), &all_targets(), "other".to_string(), None),
        RoutedModel {
            model: SOL.to_string(),
            effort: Some(ReasoningEffort::Medium),
            label: "Sol (medium)",
            outcome: RouteOutcome::Classified,
        }
    );
}

#[test]
fn invalid_score_or_threshold_falls_back_to_sol() {
    let mut invalid = config();
    invalid.low_threshold = 0.8;
    assert_eq!(
        route_score(0.1, &invalid, &all_targets(), "other".to_string(), None),
        RoutedModel {
            model: SOL.to_string(),
            effort: Some(ReasoningEffort::Medium),
            label: "Sol (medium)",
            outcome: RouteOutcome::ClassifierFallback,
        }
    );
    assert_eq!(
        route_score(
            f64::NAN,
            &config(),
            &all_targets(),
            "other".to_string(),
            None
        )
        .model,
        SOL
    );
}

#[test]
fn classifier_response_must_be_strict_json_with_a_score() {
    assert_eq!(
        serde_json::from_str::<Classification>(r#"{"score":0.5}"#)
            .expect("valid response")
            .score,
        0.5
    );
    assert!(serde_json::from_str::<Classification>("medium").is_err());
    assert!(serde_json::from_str::<Classification>(r#"{"reason":"medium"}"#).is_err());
}

#[test]
fn only_plain_http_loopback_endpoints_are_allowed() {
    assert!(is_local_endpoint("http://localhost:11434/api/generate"));
    assert!(is_local_endpoint("http://127.0.0.1:11434/api/generate"));
    assert!(is_local_endpoint("http://[::1]:11434/api/generate"));
    assert!(!is_local_endpoint("https://localhost/api/generate"));
    assert!(!is_local_endpoint("http://example.com/api/generate"));
    assert!(!is_local_endpoint("http://user@localhost/api/generate"));
}

#[test]
fn timeout_is_hard_capped() {
    assert_eq!(5_000_u64.min(MAX_TIMEOUT_MS), MAX_TIMEOUT_MS);
}

#[tokio::test]
async fn warmup_preloads_classifier_without_generating_tokens() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/generate"))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "model": "local",
            "stream": false,
            "keep_alive": "5m",
        })))
        .respond_with(
            wiremock::ResponseTemplate::new(/*status_code*/ 200).set_body_json(serde_json::json!({
                "response": "",
                "done": true,
            })),
        )
        .expect(/*n*/ 1)
        .mount(&server)
        .await;
    let mut config = config();
    config.endpoint = format!("{}/api/generate", server.uri());

    warm(&config).await.expect("warm classifier");
}
