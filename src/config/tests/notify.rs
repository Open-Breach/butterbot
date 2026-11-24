//! Tasks 1.1 to 1.3 of `add-webhook-notifier`: the `webhook` notifier type in
//! the configuration.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use super::*;
use crate::events::{EventKind, Selection};

fn webhook(config: &Config, name: &str) -> Webhook {
    let NotifierKind::Webhook(webhook) = &config.notify[name].kind;
    webhook.clone()
}

/// The webhook instance `hook` of a document that has only that instance.
fn only_instance(settings: &str) -> Webhook {
    let config = from_toml(&format!("[notify.hook]\ntype = \"webhook\"\n{settings}"))
        .unwrap_or_else(|error| panic!("{error}"));
    webhook(&config, "hook")
}

/// The problems of the webhook instance `hook` with the given settings.
fn instance_problems(settings: &str) -> Vec<String> {
    problems(&format!("[notify.hook]\ntype = \"webhook\"\n{settings}"))
}

// webhook-notifier, "Webhook notifier type"; "Method and default body": the
// defaults of an instance that has only its type.
#[test]
fn minimal_instance_takes_the_built_in_values() {
    let config = from_toml("[notify.teams]\ntype = \"webhook\"\n").expect("valid");

    assert_eq!(
        config.notify["teams"],
        NotifyInstance {
            events: Selection::All,
            kind: NotifierKind::Webhook(Webhook {
                url: None,
                method: WebhookMethod::Post,
                body: WebhookBody::Json,
                headers: BTreeMap::new(),
                access_key: None,
                ca_file: None,
                timeout: Duration::from_secs(10),
            }),
        }
    );
    assert_eq!(config.notify["teams"].kind.type_name(), "webhook");
}

#[test]
fn full_instance_is_read_into_the_model() {
    let config = from_toml(
        r#"
        [notify.console]
        type = "webhook"
        events = ["failure", "expiring"]
        url = "https://console.example.com/alarm/{{event}}"
        method = "POST"
        body = "{{summary}}\n{{details}}"
        content_type = "text/plain"
        access_key = "key-in-the-file"
        ca_file = "/etc/butterbot/console-ca.pem"
        timeout = "5s"

        [notify.console.headers]
        X-API-KEY = "{{access_key}}"
        X-Source = "butterbot"

        [notify.teams]
        type = "webhook"
        format = "teams"
        "#,
    )
    .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(
        config.notify.keys().collect::<Vec<_>>(),
        ["console", "teams"]
    );
    assert_eq!(
        config.notify["console"].events,
        Selection::only([EventKind::Failure, EventKind::Expiring])
    );
    let console = webhook(&config, "console");
    assert_eq!(
        console.url.as_ref().map(Secret::expose),
        Some("https://console.example.com/alarm/{{event}}")
    );
    assert_eq!(console.method, WebhookMethod::Post);
    assert_eq!(
        console.body,
        WebhookBody::Custom {
            template: "{{summary}}\n{{details}}".into(),
            content_type: "text/plain".into(),
        }
    );
    let headers: Vec<(&str, &str)> = console
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.expose()))
        .collect();
    assert_eq!(headers.len(), 2);
    assert!(
        headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("x-api-key")
                && *value == "{{access_key}}")
    );
    assert!(
        headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("x-source") && *value == "butterbot")
    );
    assert_eq!(
        console.access_key.as_ref().map(Secret::expose),
        Some("key-in-the-file")
    );
    assert_eq!(
        console.ca_file,
        Some(PathBuf::from("/etc/butterbot/console-ca.pem"))
    );
    assert_eq!(console.timeout, Duration::from_secs(5));

    assert_eq!(config.notify["teams"].events, Selection::All);
    assert_eq!(webhook(&config, "teams").body, WebhookBody::Teams);
}

#[test]
fn custom_body_is_json_unless_a_content_type_is_given() {
    assert_eq!(
        only_instance("body = '{\"message\": \"{{summary}}\"}'\n").body,
        WebhookBody::Custom {
            template: r#"{"message": "{{summary}}"}"#.into(),
            content_type: "application/json".into(),
        }
    );
}

// webhook-notifier, "Teams format": `format = "json"` selects the default body.
#[test]
fn format_json_is_the_default_body() {
    assert_eq!(only_instance("format = \"json\"\n").body, WebhookBody::Json);
    assert_eq!(only_instance("").body, WebhookBody::Json);
    assert_eq!(
        only_instance("format = \"teams\"\n").body,
        WebhookBody::Teams
    );
}

// webhook-notifier, "Method and default body", scenario "GET request": the setting.
#[test]
fn method_get_is_accepted() {
    assert_eq!(
        only_instance("method = \"GET\"\n").method,
        WebhookMethod::Get
    );
}

#[test]
fn unknown_key_in_a_webhook_instance_is_rejected() {
    assert_eq!(
        instance_problems("retries = 5\n"),
        ["notify.hook: unknown key `retries` (set in test.toml)"]
    );
}

// run-events, "Notifier extension point", scenario "Unknown notifier type".
#[test]
fn unknown_notifier_type_names_the_instance_and_the_types_that_exist() {
    assert_eq!(
        problems("[notify.birds]\ntype = \"carrier-pigeon\"\nloft = \"north\"\n"),
        ["notify.birds: unknown type \"carrier-pigeon\" (known types: webhook)"]
    );
}

#[test]
fn notifier_without_a_type_is_rejected() {
    assert_eq!(
        problems("[notify.hook]\nurl = \"https://hooks.example.com/a\"\n"),
        ["notify.hook: missing key `type`"]
    );
}

// --- Task 1.2: one test per rule -------------------------------------------

#[test]
fn method_has_to_be_post_or_get() {
    for method in ["PUT", "DELETE", "post", ""] {
        let found = instance_problems(&format!("method = \"{method}\"\n"));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0].starts_with(&format!(
                "notify.hook: `method`: unknown method {method:?} (expected one of: POST, GET)"
            )),
            "{found:#?}"
        );
    }
}

#[test]
fn format_has_to_be_json_or_teams() {
    let found = instance_problems("format = \"slack\"\n");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].starts_with(
            "notify.hook: `format`: unknown format \"slack\" (expected one of: json, teams)"
        ),
        "{found:#?}"
    );
}

// webhook-notifier, "Custom body", scenario "Body and format together".
#[test]
fn body_and_format_together_are_rejected() {
    for format in ["teams", "json"] {
        let found = instance_problems(&format!(
            "format = \"{format}\"\nbody = \"{{{{summary}}}}\"\n"
        ));
        assert_eq!(
            found,
            [
                "notify.hook: `body`: cannot be set together with `format`: a notifier has either \
                 a custom body or a built-in format"
            ]
        );
    }
}

#[test]
fn header_names_have_to_be_valid() {
    for name in ["X API KEY", "X-Key:", "", "Schlüssel"] {
        let found = instance_problems(&format!(
            "[notify.hook.headers]\n\"{name}\" = \"header-value-1\"\n"
        ));
        assert_eq!(found.len(), 1, "{name:?}: {found:#?}");
        assert!(
            found[0].starts_with(&format!(
                "notify.hook: `headers`: {name:?} is not a valid header name"
            )),
            "{found:#?}"
        );
        assert!(!found[0].contains("header-value-1"), "{found:#?}");
    }
}

#[test]
fn header_values_have_to_be_text_and_headers_a_table() {
    let found = instance_problems("[notify.hook.headers]\nX-Count = 3\n");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].starts_with(
            "notify.hook: `headers`: the value of the header X-Count has to be a string, found a \
             whole number"
        ) || found[0].starts_with(
            "notify.hook: `headers`: the value of the header x-count has to be a string, found a \
             whole number"
        ),
        "{found:#?}"
    );

    let found = instance_problems("headers = \"X-API-KEY: value\"\n");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].starts_with(
            "notify.hook: `headers`: expected a table of header names and values, found a string"
        ),
        "{found:#?}"
    );
}

#[test]
fn timeout_is_a_duration_of_at_least_a_second() {
    assert_eq!(
        only_instance("timeout = \"3s\"\n").timeout,
        Duration::from_secs(3)
    );
    for (timeout, expected) in [
        ("soon", "invalid duration"),
        ("0s", "is too short: the minimum is 1s"),
    ] {
        let found = instance_problems(&format!("timeout = \"{timeout}\"\n"));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0].starts_with("notify.hook: `timeout`: ") && found[0].contains(expected),
            "{found:#?}"
        );
    }
}

#[test]
fn every_problem_of_an_instance_is_reported() {
    let found = instance_problems(
        "method = \"PUT\"\nformat = \"slack\"\ntimeout = \"soon\"\nevents = [\"renewal\"]\n",
    );
    assert_eq!(found.len(), 4, "{found:#?}");
}

// run-events, "Notifier extension point", scenario "Notifier with a filter":
// the selection as it is read.
#[test]
fn events_selects_the_kinds_a_notifier_receives() {
    let config =
        from_toml("[notify.hook]\ntype = \"webhook\"\nevents = [\"failure\", \"expiring\"]\n")
            .expect("valid");
    let selection = &config.notify["hook"].events;
    assert!(selection.includes(EventKind::Failure));
    assert!(selection.includes(EventKind::Expiring));
    assert!(!selection.includes(EventKind::Renewed));
    assert!(!selection.includes(EventKind::RunCompleted));

    let found = instance_problems("events = [\"failure\", \"renewal\"]\n");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].starts_with("notify.hook: `events`: unknown event \"renewal\""),
        "{found:#?}"
    );
}

// --- Task 1.3: the URL and the key are secrets by path -----------------------

fn listing(text: &str, pairs: &[(&str, &str)]) -> Vec<ExpectedSecret> {
    from_toml_and_env(text, pairs)
        .unwrap_or_else(|error| panic!("{error}"))
        .expected_secrets()
}

// webhook-notifier, "Target URL is a secret": the URL is identified by its path
// and may come from any layer; "Notifier without a URL is skipped": the path is
// listed as missing.
#[test]
fn url_is_an_expected_secret_with_its_source_names() {
    let text = "[notify.teams]\ntype = \"webhook\"\nformat = \"teams\"\n";

    assert_eq!(
        listing(text, &[]),
        [ExpectedSecret {
            path: "notify.teams.url".into(),
            env_name: "BUTTERBOT__NOTIFY__TEAMS__URL".into(),
            vault_name: "notify--teams--url".into(),
            found: false,
        }]
    );

    let supplied = from_toml_and_env(
        text,
        &[(
            "BUTTERBOT__NOTIFY__TEAMS__URL",
            "https://hooks.example.com/from-the-environment",
        )],
    )
    .expect("valid");
    assert!(supplied.expected_secrets()[0].found);
    assert_eq!(
        webhook(&supplied, "teams").url.as_ref().map(Secret::expose),
        Some("https://hooks.example.com/from-the-environment")
    );
}

#[test]
fn an_empty_url_counts_as_not_supplied() {
    let config = from_toml("[notify.hook]\ntype = \"webhook\"\nurl = \"\"\n").expect("valid");
    assert_eq!(webhook(&config, "hook").url, None);
    assert!(!config.expected_secrets()[0].found);
}

// webhook-notifier, "Headers and access key": the key is required when a
// template uses it, and only then.
#[test]
fn access_key_is_expected_only_when_a_template_uses_it() {
    let paths = |settings: &str| -> Vec<String> {
        listing(
            &format!("[notify.console]\ntype = \"webhook\"\n{settings}"),
            &[],
        )
        .into_iter()
        .map(|secret| secret.path)
        .collect()
    };

    assert_eq!(paths(""), ["notify.console.url"]);
    assert_eq!(
        paths("access_key = \"set-but-not-used\"\n"),
        ["notify.console.url"]
    );
    for uses_the_key in [
        "[notify.console.headers]\nX-API-KEY = \"{{access_key}}\"\n",
        "[notify.console.headers]\nAuthorization = \"Bearer {{access_key}}\"\n",
        "url = \"https://console.example.com/hook?key={{access_key}}\"\n",
        "body = '{\"key\": \"{{access_key}}\", \"text\": \"{{summary}}\"}'\n",
    ] {
        assert_eq!(
            paths(uses_the_key),
            ["notify.console.url", "notify.console.access_key"],
            "{uses_the_key}"
        );
    }
}

// webhook-notifier, "Headers and access key", scenario "Key referenced but
// missing": the listing.
#[test]
fn referenced_access_key_is_listed_with_its_source_names_and_whether_found() {
    let text = "[notify.console]\ntype = \"webhook\"\n\n[notify.console.headers]\nX-API-KEY = \
                \"{{access_key}}\"\n";

    let missing = listing(text, &[]);
    assert_eq!(
        missing[1],
        ExpectedSecret {
            path: "notify.console.access_key".into(),
            env_name: "BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY".into(),
            vault_name: "notify--console--access-key".into(),
            found: false,
        }
    );

    let found = listing(
        text,
        &[("BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY", "key-from-the-env")],
    );
    assert!(found[1].found);
}

// webhook-notifier, "Nothing secret in the output": the configuration and the
// listing can be printed without showing the URL, the key or a header value.
#[test]
fn neither_the_url_nor_the_key_nor_a_header_value_is_printed() {
    let config = from_toml_and_env(
        r#"
        [notify.console]
        type = "webhook"
        url = "https://console.example.com/hook/path-with-a-credential"

        [notify.console.headers]
        X-API-KEY = "{{access_key}}"
        Authorization = "Bearer written-into-the-header"
        "#,
        &[("BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY", "key-from-the-env")],
    )
    .unwrap_or_else(|error| panic!("{error}"));

    let printed = format!("{config:?}\n{config:#?}\n{:?}", config.expected_secrets());
    for value in [
        "console.example.com",
        "path-with-a-credential",
        "key-from-the-env",
        "written-into-the-header",
    ] {
        assert!(!printed.contains(value), "{value} is printed");
    }
    assert!(printed.contains(PLACEHOLDER));
}

#[test]
fn a_problem_with_the_url_does_not_show_a_value() {
    let found = instance_problems("url = 17\naccess_key = 23\n");
    assert_eq!(
        found,
        [
            "notify.hook: `url`: expected a string, found a whole number (set in test.toml)",
            "notify.hook: `access_key`: expected a string, found a whole number (set in test.toml)",
        ]
    );
}

#[test]
fn a_configuration_without_a_notifier_expects_no_notifier_secret() {
    let config = from_toml("").expect("valid");
    assert!(config.notify.is_empty());
    assert!(config.expected_secrets().is_empty());
}
