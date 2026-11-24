//! Tasks 2.1 to 2.3 of `add-webhook-notifier`: parsing and rendering templates.

use serde_json::{Value, json};
use time::macros::datetime;

use super::*;
use crate::events::{Failure, examples};

fn template(text: &str) -> Template {
    Template::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn render(text: &str, position: Position, event: &Event) -> String {
    template(text).render(position, &Values::of(event, "key-1"))
}

/// A failure whose reason has everything that needs escaping somewhere: a
/// quotation mark, a backslash, a line break, non-ASCII text, and characters
/// with a meaning in a URL.
fn awkward_failure() -> Event {
    Event::Failure(Failure {
        timestamp: datetime!(2026-11-23 02:00:38 UTC),
        certificate: "wildcard-example-com".into(),
        step: "validate".into(),
        reason: AWKWARD_REASON.into(),
        stored_expiry: None,
    })
}

const AWKWARD_REASON: &str = "the server said \"no\" for C:\\zones\nZertifikat für *.example.com \
                              abgelehnt: ungültig & 50% ?/#";

fn one_of_each() -> [Event; 4] {
    [
        examples::run_completed(),
        examples::failure(),
        examples::renewed(),
        examples::expiring(),
    ]
}

// --- Task 2.1: parsing ----------------------------------------------------------

// webhook-notifier, "Placeholders": the names that are supported.
#[test]
fn every_placeholder_is_parsed() {
    for placeholder in Placeholder::ALL {
        let text = format!("before {{{{{}}}}} after", placeholder.name());
        let parsed = template(&text);
        for other in Placeholder::ALL {
            assert_eq!(parsed.uses(other), other == placeholder, "{text}");
        }
    }
    assert_eq!(
        Placeholder::names(),
        "event, summary, details, exit_code, event_json, access_key"
    );
}

// webhook-notifier, "Missing and unknown placeholders", scenario "Unknown
// placeholder": the template itself. That the configuration is rejected is
// tested in `config::tests::notify`.
#[test]
fn unknown_placeholder_is_an_error_that_names_it() {
    let error = Template::parse("{\"text\": \"{{certificat}}\"}").expect_err("unknown");
    assert_eq!(
        error,
        TemplateError::UnknownPlaceholder("certificat".into())
    );
    assert_eq!(error.to_string(), "unknown placeholder `{{certificat}}`");
}

#[test]
fn placeholder_that_is_not_closed_is_an_error() {
    for text in ["{{summary", "a {{", "{{summary}} and {{event"] {
        assert_eq!(
            Template::parse(text),
            Err(TemplateError::Unclosed),
            "{text}"
        );
    }
}

#[test]
fn text_without_placeholders_is_a_template_of_itself() {
    for text in [
        "",
        "plain",
        "{\"a\": {\"b\": 1}}",
        "single { and } braces",
        "}} closing first",
    ] {
        assert_eq!(
            render(text, Position::OtherBody, &examples::failure()),
            text
        );
    }
}

#[test]
fn placeholders_may_touch_each_other_and_repeat() {
    assert_eq!(
        render(
            "{{event}}{{event}}:{{exit_code}}{{event}}",
            Position::OtherBody,
            &examples::renewed()
        ),
        "renewedrenewed:renewed"
    );
}

#[test]
fn literal_start_is_the_text_before_the_first_placeholder() {
    assert_eq!(
        template("https://hooks.example.com/{{event}}/x").literal_start(),
        "https://hooks.example.com/"
    );
    assert_eq!(template("{{event}}/x").literal_start(), "");
    assert_eq!(template("").literal_start(), "");
}

#[test]
fn debug_output_does_not_show_the_text() {
    let shown = format!(
        "{:?}",
        template("https://hooks.example.com/credential/{{event}}")
    );
    assert_eq!(shown, "[template]");
}

// --- Task 2.2: rendering ----------------------------------------------------------

// webhook-notifier, "Placeholder values are escaped for their position",
// scenario "Reason with quotation marks".
#[test]
fn json_body_stays_valid_json_whatever_the_reason_contains() {
    let body = render(
        r#"{"text": "{{details}}", "title": "{{summary}}", "kind": "{{event}}"}"#,
        Position::JsonBody,
        &awkward_failure(),
    );

    let parsed: Value =
        serde_json::from_str(&body).unwrap_or_else(|error| panic!("{error}: {body}"));
    let text = parsed["text"].as_str().expect("text");
    assert!(text.contains(AWKWARD_REASON), "{text}");
    for character in ['"', '\\', '\n', 'ü'] {
        assert!(text.contains(character), "{character:?} in {text}");
    }
    assert_eq!(
        parsed["title"],
        "certificate wildcard-example-com failed at step validate"
    );
    assert_eq!(parsed["kind"], "failure");
    assert_eq!(
        parsed.as_object().expect("an object").len(),
        3,
        "the reason added no member"
    );
}

// webhook-notifier, "Custom body", scenario "Custom JSON body": the rendering.
#[test]
fn summary_in_a_json_body_says_what_failed() {
    let body = render(
        r#"{"message": "{{summary}}"}"#,
        Position::JsonBody,
        &awkward_failure(),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&body).expect("valid JSON"),
        json!({ "message": "certificate wildcard-example-com failed at step validate" })
    );
}

// webhook-notifier, "Placeholder values are escaped for their position":
// `event_json` is inserted into a body as a JSON object.
#[test]
fn event_json_is_inserted_into_a_body_as_an_object() {
    let event = awkward_failure();
    for position in [Position::JsonBody, Position::OtherBody] {
        let body = render(
            r#"{"source": "butterbot", "event": {{event_json}}}"#,
            position,
            &event,
        );
        let parsed: Value =
            serde_json::from_str(&body).unwrap_or_else(|error| panic!("{error}: {body}"));
        assert_eq!(
            parsed["event"],
            Value::Object(event.to_json()),
            "{position:?}"
        );
        assert_eq!(parsed["event"]["reason"], AWKWARD_REASON);
    }
    assert_eq!(
        render("{{event_json}}", Position::JsonBody, &examples::expiring()),
        r#"{"event":"expiring","timestamp":"2026-11-23T02:00:38Z","certificate":"api-example-org","expiry":"2026-12-05T08:15:00Z"}"#
    );
}

#[test]
fn value_in_another_body_is_inserted_as_it_is() {
    let body = render(
        "{{summary}}\n\n{{details}}",
        Position::OtherBody,
        &awkward_failure(),
    );
    assert_eq!(
        body,
        format!(
            "certificate wildcard-example-com failed at step validate\n\n\
             timestamp: 2026-11-23T02:00:38Z\n\
             certificate: wildcard-example-com\n\
             step: validate\n\
             reason: {AWKWARD_REASON}\n\
             stored_expiry: none"
        )
    );
}

// webhook-notifier, "Placeholder values are escaped for their position":
// percent-encoded in a URL.
#[test]
fn value_in_a_url_is_percent_encoded() {
    let url = render(
        "https://hooks.example.com/notify/{{event}}?text={{summary}}&key={{access_key}}",
        Position::Url,
        &examples::failure(),
    );
    assert_eq!(
        url,
        "https://hooks.example.com/notify/failure?text=certificate%20api-example-org%20failed%20\
         at%20step%20validate&key=key-1"
    );

    let encoded = render("{{details}}", Position::Url, &awkward_failure());
    assert!(
        encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~%".contains(&byte)),
        "{encoded}"
    );
    for (character, code) in [
        ("\"", "%22"),
        ("\\", "%5C"),
        ("\n", "%0A"),
        ("ü", "%C3%BC"),
        ("&", "%26"),
        ("%", "%25"),
        ("?", "%3F"),
        ("/", "%2F"),
        ("#", "%23"),
        ("*", "%2A"),
        (":", "%3A"),
        (" ", "%20"),
    ] {
        assert!(
            encoded.contains(code),
            "{character:?} is written as {code}: {encoded}"
        );
    }

    let with_key = template("https://hooks.example.com/?key={{access_key}}").render(
        Position::Url,
        &Values::of(&examples::failure(), "a/b&c=d e"),
    );
    assert_eq!(with_key, "https://hooks.example.com/?key=a%2Fb%26c%3Dd%20e");
}

#[test]
fn value_in_a_header_is_inserted_as_it_is() {
    assert_eq!(
        render(
            "Bearer {{access_key}}",
            Position::Header,
            &examples::failure()
        ),
        "Bearer key-1"
    );
    assert_eq!(
        render("{{summary}}", Position::Header, &awkward_failure()),
        "certificate wildcard-example-com failed at step validate"
    );
    // A line break stays a line break. The sink refuses to send such a value.
    assert!(render("{{details}}", Position::Header, &awkward_failure()).contains('\n'));
}

#[test]
fn body_position_follows_the_content_type() {
    for json in [
        "application/json",
        "Application/JSON",
        "application/json; charset=utf-8",
        "application/problem+json",
        "application/vnd.example.alert+json;version=2",
    ] {
        assert_eq!(Position::of_body(json), Position::JsonBody, "{json}");
    }
    for other in [
        "text/plain",
        "text/plain; charset=utf-8",
        "application/x-www-form-urlencoded",
        "application/jsonl",
        "",
    ] {
        assert_eq!(Position::of_body(other), Position::OtherBody, "{other}");
    }
}

// --- Task 2.3: `details` and `exit_code` ------------------------------------------

#[test]
fn details_are_the_fields_of_the_event_in_the_order_of_its_json() {
    assert_eq!(
        details(&examples::run_completed()),
        "timestamp: 2026-11-23T02:00:41Z\n\
         outcome: failed\n\
         checked: 15\n\
         renewed: 2\n\
         failed: 1\n\
         duration_seconds: 41\n\
         soonest_certificate: api-example-org\n\
         soonest_expiry: 2026-12-05T08:15:00Z"
    );
    assert_eq!(
        details(&examples::failure()),
        "timestamp: 2026-11-23T02:00:38Z\n\
         certificate: api-example-org\n\
         step: validate\n\
         reason: the server rejected the challenge for api.example.org: incorrect TXT record\n\
         stored_expiry: 2026-12-05T08:15:00Z"
    );
    assert_eq!(
        details(&examples::renewed()),
        "timestamp: 2026-11-23T02:00:35Z\n\
         certificate: wildcard-example-com\n\
         names: *.example.com, example.com\n\
         expiry: 2027-02-21T01:00:34Z\n\
         acme: letsencrypt\n\
         stores: vault, local"
    );
    assert_eq!(
        details(&examples::expiring()),
        "timestamp: 2026-11-23T02:00:38Z\n\
         certificate: api-example-org\n\
         expiry: 2026-12-05T08:15:00Z"
    );
}

#[test]
fn field_without_a_value_is_shown_as_none() {
    assert_eq!(
        details(&examples::run_completed_nothing_due()),
        "timestamp: 2026-11-23T02:00:03Z\n\
         outcome: success\n\
         checked: 0\n\
         renewed: 0\n\
         failed: 0\n\
         duration_seconds: 3\n\
         soonest_certificate: none\n\
         soonest_expiry: none"
    );
    assert!(details(&examples::failure_nothing_stored()).ends_with("\nstored_expiry: none"));
}

#[test]
fn details_have_every_field_of_the_event_except_its_kind() {
    for event in one_of_each() {
        let names: Vec<String> = fields(&event).into_iter().map(|(name, _)| name).collect();
        let expected: Vec<String> = event
            .to_json()
            .keys()
            .filter(|name| *name != "event")
            .cloned()
            .collect();
        assert_eq!(names, expected, "{}", event.kind());
        assert_eq!(details(&event).lines().count(), expected.len());
    }
}

// webhook-notifier, "Placeholders", scenarios "Exit code in the URL" and
// "Successful run"; "Missing and unknown placeholders", scenario
// "No value for this event". The requests themselves are tested with the sink.
#[test]
fn exit_code_is_that_of_the_run_and_empty_for_other_events() {
    use crate::cli::Exit;

    assert_eq!(exit_code(&examples::run_completed()), "1");
    assert_eq!(exit_code(&examples::run_completed_nothing_due()), "0");
    assert_eq!(
        exit_code(&examples::run_completed()),
        Exit::Failed.code().to_string()
    );
    assert_eq!(
        exit_code(&examples::run_completed_nothing_due()),
        Exit::Success.code().to_string()
    );
    for event in [
        examples::failure(),
        examples::failure_nothing_stored(),
        examples::renewed(),
        examples::expiring(),
    ] {
        assert_eq!(exit_code(&event), "", "{}", event.kind());
    }

    let url = "https://hooks.example.com/ping/{{exit_code}}";
    assert_eq!(
        render(url, Position::Url, &examples::run_completed()),
        "https://hooks.example.com/ping/1"
    );
    assert_eq!(
        render(url, Position::Url, &examples::run_completed_nothing_due()),
        "https://hooks.example.com/ping/0"
    );
    assert_eq!(
        render(url, Position::Url, &examples::renewed()),
        "https://hooks.example.com/ping/"
    );
}

#[test]
fn every_placeholder_has_its_value_for_every_kind_of_event() {
    for event in one_of_each() {
        let rendered = render(
            "{{event}}|{{summary}}|{{exit_code}}|{{access_key}}",
            Position::OtherBody,
            &event,
        );
        assert_eq!(
            rendered,
            format!(
                "{}|{}|{}|key-1",
                event.kind(),
                event.summary(),
                exit_code(&event)
            )
        );
    }
}
