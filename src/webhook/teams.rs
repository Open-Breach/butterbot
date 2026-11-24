//! The body for a Microsoft Teams workflow (`format = "teams"`).
//!
//! The receiver is a workflow of the Workflows app (Power Automate) that is
//! started by the trigger "When a Teams webhook request is received" and
//! posts the card it is sent to a chat or channel.
//!
//! Source of the payload, read on 2026-11-24:
//!
//! - Microsoft Learn, connector reference "Microsoft Teams", trigger "When a
//!   Teams webhook request is received", sections "Request Body (Adaptive
//!   cards)" and "Request Body Example (Adaptive cards)":
//!   <https://learn.microsoft.com/en-us/connectors/teams/>
//! - Microsoft Learn, the article on incoming webhooks with workflows:
//!   <https://learn.microsoft.com/en-us/microsoftteams/platform/webhooks-and-connectors/how-to/add-incoming-webhook>
//! - Microsoft Learn, the article on sending messages to a webhook:
//!   <https://learn.microsoft.com/en-us/microsoftteams/platform/webhooks-and-connectors/how-to/connectors-using>
//!
//! What those pages state, and what this module follows:
//!
//! - The body is `{"type": "message", "attachments": [...]}`. `type` is
//!   required and is always `message`.
//! - An attachment for an Adaptive Card has the `contentType`
//!   `application/vnd.microsoft.card.adaptive`, a `contentUrl` that is always
//!   `null`, and the card as `content`.
//! - The examples use the card schema
//!   `http://adaptivecards.io/schemas/adaptive-card.json` in version `1.2`.
//! - A message may be at most about 28 KB.
//! - A workflow whose trigger allows "Anyone" must not be sent an
//!   authentication header; the other two options need a Microsoft token,
//!   which Butterbot cannot present.
//! - The text of a card is read as basic Markdown.
//! - All elements of the Adaptive Card schema are supported, except the
//!   action `Action.Submit`.
//!
//! The examples on those pages have a single `TextBlock` with a `text`. The
//! card built here also uses the `weight` and `wrap` of a `TextBlock` and a
//! `FactSet`, which are elements of the card schema that no example on those
//! pages shows.
//!
//! What the pages do not state: the status a workflow answers with, and how a
//! `*` or `_` in a value is shown. Values are sent as they are. The connector
//! incoming webhooks of Office 365, for which many examples on the web were
//! written, were retired in May 2026 according to the second page.

use serde_json::{Value, json};

use super::template::field_text;
use crate::events::Event;

/// The content type of an Adaptive Card attachment.
const CARD_CONTENT_TYPE: &str = "application/vnd.microsoft.card.adaptive";
const CARD_SCHEMA: &str = "http://adaptivecards.io/schemas/adaptive-card.json";
const CARD_VERSION: &str = "1.2";

/// The message for `event`: one Adaptive Card with the event's summary as its
/// heading and one fact per field of the event.
pub fn message(event: &Event) -> Value {
    let facts: Vec<Value> = event
        .to_json()
        .iter()
        .map(|(name, value)| json!({ "title": name, "value": field_text(value) }))
        .collect();
    json!({
        "type": "message",
        "attachments": [{
            "contentType": CARD_CONTENT_TYPE,
            "contentUrl": null,
            "content": {
                "$schema": CARD_SCHEMA,
                "type": "AdaptiveCard",
                "version": CARD_VERSION,
                "body": [
                    {
                        "type": "TextBlock",
                        "text": event.summary(),
                        "weight": "Bolder",
                        "wrap": true
                    },
                    {
                        "type": "FactSet",
                        "facts": facts
                    }
                ]
            }
        }]
    })
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::events::{Failure, examples};

    fn card(message: &Value) -> &Value {
        &message["attachments"][0]["content"]
    }

    fn facts(message: &Value) -> Vec<(String, String)> {
        card(message)["body"][1]["facts"]
            .as_array()
            .expect("the facts")
            .iter()
            .map(|fact| {
                (
                    fact["title"].as_str().expect("a title").to_owned(),
                    fact["value"].as_str().expect("a value").to_owned(),
                )
            })
            .collect()
    }

    // Snapshots. Each is the whole body, as it is sent.

    // webhook-notifier, "Teams format", scenario "Failure posted to Teams".
    #[test]
    fn snapshot_failure() {
        assert_eq!(
            message(&examples::failure()).to_string(),
            r#"{"type":"message","attachments":[{"contentType":"application/vnd.microsoft.card.adaptive","contentUrl":null,"content":{"$schema":"http://adaptivecards.io/schemas/adaptive-card.json","type":"AdaptiveCard","version":"1.2","body":[{"type":"TextBlock","text":"certificate api-example-org failed at step validate","weight":"Bolder","wrap":true},{"type":"FactSet","facts":[{"title":"event","value":"failure"},{"title":"timestamp","value":"2026-11-23T02:00:38Z"},{"title":"certificate","value":"api-example-org"},{"title":"step","value":"validate"},{"title":"reason","value":"the server rejected the challenge for api.example.org: incorrect TXT record"},{"title":"stored_expiry","value":"2026-12-05T08:15:00Z"}]}]}}]}"#
        );
    }

    #[test]
    fn snapshot_run_completed() {
        assert_eq!(
            message(&examples::run_completed()).to_string(),
            r#"{"type":"message","attachments":[{"contentType":"application/vnd.microsoft.card.adaptive","contentUrl":null,"content":{"$schema":"http://adaptivecards.io/schemas/adaptive-card.json","type":"AdaptiveCard","version":"1.2","body":[{"type":"TextBlock","text":"run completed: 15 checked, 2 renewed, 1 failed","weight":"Bolder","wrap":true},{"type":"FactSet","facts":[{"title":"event","value":"run_completed"},{"title":"timestamp","value":"2026-11-23T02:00:41Z"},{"title":"outcome","value":"failed"},{"title":"checked","value":"15"},{"title":"renewed","value":"2"},{"title":"failed","value":"1"},{"title":"duration_seconds","value":"41"},{"title":"soonest_certificate","value":"api-example-org"},{"title":"soonest_expiry","value":"2026-12-05T08:15:00Z"}]}]}}]}"#
        );
    }

    #[test]
    fn snapshot_renewed() {
        assert_eq!(
            message(&examples::renewed()).to_string(),
            r#"{"type":"message","attachments":[{"contentType":"application/vnd.microsoft.card.adaptive","contentUrl":null,"content":{"$schema":"http://adaptivecards.io/schemas/adaptive-card.json","type":"AdaptiveCard","version":"1.2","body":[{"type":"TextBlock","text":"certificate wildcard-example-com renewed, expires 2027-02-21T01:00:34Z","weight":"Bolder","wrap":true},{"type":"FactSet","facts":[{"title":"event","value":"renewed"},{"title":"timestamp","value":"2026-11-23T02:00:35Z"},{"title":"certificate","value":"wildcard-example-com"},{"title":"names","value":"*.example.com, example.com"},{"title":"expiry","value":"2027-02-21T01:00:34Z"},{"title":"acme","value":"letsencrypt"},{"title":"stores","value":"vault, local"}]}]}}]}"#
        );
    }

    #[test]
    fn snapshot_expiring() {
        assert_eq!(
            message(&examples::expiring()).to_string(),
            r#"{"type":"message","attachments":[{"contentType":"application/vnd.microsoft.card.adaptive","contentUrl":null,"content":{"$schema":"http://adaptivecards.io/schemas/adaptive-card.json","type":"AdaptiveCard","version":"1.2","body":[{"type":"TextBlock","text":"certificate api-example-org was not renewed and expires 2026-12-05T08:15:00Z","weight":"Bolder","wrap":true},{"type":"FactSet","facts":[{"title":"event","value":"expiring"},{"title":"timestamp","value":"2026-11-23T02:00:38Z"},{"title":"certificate","value":"api-example-org"},{"title":"expiry","value":"2026-12-05T08:15:00Z"}]}]}}]}"#
        );
    }

    // webhook-notifier, "Teams format": a message carrying one Adaptive Card
    // that shows the summary as its heading and each field as a name and a value.
    #[test]
    fn message_carries_one_card_with_the_summary_and_every_field() {
        for event in [
            examples::run_completed(),
            examples::run_completed_nothing_due(),
            examples::failure(),
            examples::failure_nothing_stored(),
            examples::renewed(),
            examples::expiring(),
        ] {
            let message = message(&event);
            assert_eq!(message["type"], "message");
            let attachments = message["attachments"].as_array().expect("attachments");
            assert_eq!(attachments.len(), 1, "one card");
            assert_eq!(attachments[0]["contentType"], CARD_CONTENT_TYPE);
            assert!(attachments[0]["contentUrl"].is_null());
            let card = card(&message);
            assert_eq!(card["type"], "AdaptiveCard");
            assert_eq!(card["body"][0]["type"], "TextBlock");
            assert_eq!(card["body"][0]["text"], event.summary());
            assert_eq!(card["body"][0]["wrap"], true);
            assert_eq!(card["body"][1]["type"], "FactSet");

            let names: Vec<String> = facts(&message).into_iter().map(|(name, _)| name).collect();
            let expected: Vec<String> = event.to_json().keys().cloned().collect();
            assert_eq!(names, expected, "the facts of {}", event.kind());
        }
    }

    #[test]
    fn field_without_a_value_is_shown_as_none() {
        let facts = facts(&message(&examples::failure_nothing_stored()));
        assert_eq!(
            facts.last(),
            Some(&("stored_expiry".to_owned(), "none".to_owned()))
        );
    }

    #[test]
    fn reason_is_carried_as_it_is_whatever_it_contains() {
        let reason = "the server said \"no\" for C:\\zones\nZertifikat für *.example.com abgelehnt";
        let event = Event::Failure(Failure {
            timestamp: datetime!(2026-11-23 02:00:38 UTC),
            certificate: "wildcard-example-com".into(),
            step: "validate".into(),
            reason: reason.into(),
            stored_expiry: None,
        });

        let sent = message(&event).to_string();

        let parsed: Value = serde_json::from_str(&sent).expect("valid JSON");
        assert!(
            facts(&parsed).contains(&("reason".to_owned(), reason.to_owned())),
            "{sent}"
        );
    }

    #[test]
    fn message_is_far_below_the_size_limit_of_teams() {
        for event in [
            examples::run_completed(),
            examples::failure(),
            examples::renewed(),
            examples::expiring(),
        ] {
            assert!(message(&event).to_string().len() < 2_000);
        }
    }
}
