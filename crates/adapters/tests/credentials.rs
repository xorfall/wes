use std::sync::{Arc, Mutex};
use wes_adapters::credentials::{CredentialLimits, MemoryCredentials, environment_variable};
use wes_engine::credentials::{CredentialError, Credentials, ExposeSecret, SecretString};

fn secret(text: &str) -> SecretString {
    SecretString::from(text.to_owned())
}
fn empty(limits: CredentialLimits) -> MemoryCredentials {
    MemoryCredentials::with_environment(limits, |_| Ok(None))
}

#[test]
fn supplied_values_override_named_environment_fallback_and_blanks_remove_only_the_override() {
    let queried = Arc::new(Mutex::new(vec![]));
    let captured = queried.clone();
    let store = MemoryCredentials::with_environment(CredentialLimits::default(), move |name| {
        captured.lock().unwrap().push(name.to_owned());
        Ok((name == "WES_CATALOG_KEY").then(|| secret("fixture-environment")))
    });
    assert_eq!(
        store
            .lookup("catalog-key")
            .unwrap()
            .unwrap()
            .expose_secret(),
        "fixture-environment"
    );
    store
        .remember("catalog-key".into(), secret("fixture-supplied"))
        .unwrap();
    assert_eq!(
        store
            .lookup("catalog-key")
            .unwrap()
            .unwrap()
            .expose_secret(),
        "fixture-supplied"
    );
    assert_eq!(queried.lock().unwrap().len(), 1);
    store
        .remember("catalog-key".into(), secret(" \t\n"))
        .unwrap();
    assert_eq!(
        store
            .lookup("catalog-key")
            .unwrap()
            .unwrap()
            .expose_secret(),
        "fixture-environment"
    );
    assert!(store.names().unwrap().is_empty());
    assert!(store.lookup("missing").unwrap().is_none());
    assert_eq!(
        *queried.lock().unwrap(),
        ["WES_CATALOG_KEY", "WES_CATALOG_KEY", "WES_MISSING"]
    );
}

#[test]
fn debug_and_name_enumeration_never_contain_credential_values() {
    let store = empty(CredentialLimits::default());
    store
        .remember("catalog".into(), secret("FIXTURE-NOT-FOR-LOGGING"))
        .unwrap();
    assert_eq!(store.names().unwrap(), ["catalog"]);
    let value = store.lookup("catalog").unwrap().unwrap();
    let debug = format!("{store:?} {value:?}");
    assert!(debug.contains("catalog"));
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("FIXTURE-NOT-FOR-LOGGING"));
}

#[test]
fn lookups_share_immutable_snapshots_and_replacement_does_not_retarget_in_flight_credentials() {
    let store = empty(CredentialLimits::default());
    store.remember("key".into(), secret("first")).unwrap();
    let first = store.lookup("key").unwrap().unwrap();
    assert!(Arc::ptr_eq(&first, &store.lookup("key").unwrap().unwrap()));
    store.remember("key".into(), secret("second")).unwrap();
    let second = store.lookup("key").unwrap().unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    store.remember("key".into(), secret("")).unwrap();
    assert!(store.lookup("key").unwrap().is_none());
    assert_eq!(first.expose_secret(), "first");
    assert_eq!(second.expose_secret(), "second");
    // Zeroization occurs when the final secret allocation owner drops, not while a request holds it.
}

#[test]
fn all_capacity_checks_are_atomic_and_removal_releases_admitted_budget() {
    let limits = CredentialLimits {
        entries: 1,
        name_bytes: 4,
        value_bytes: 8,
        total_bytes: 6,
    };
    let store = empty(limits);
    store.remember("key".into(), secret("one")).unwrap();
    let original = store.lookup("key").unwrap().unwrap();
    assert_eq!(
        store.remember("long-name".into(), secret("x")),
        Err(CredentialError::InvalidName)
    );
    assert_eq!(
        store.remember("key".into(), secret("ninechars")),
        Err(CredentialError::TooLarge)
    );
    assert_eq!(
        store.remember("key".into(), secret("four")),
        Err(CredentialError::Capacity)
    );
    assert_eq!(
        store.remember("two".into(), secret("x")),
        Err(CredentialError::Capacity)
    );
    assert!(Arc::ptr_eq(
        &original,
        &store.lookup("key").unwrap().unwrap()
    ));
    store.remember("key".into(), secret("")).unwrap();
    store.remember("two".into(), secret("new")).unwrap();
    assert_eq!(store.names().unwrap(), ["two"]);
    store.remember("two".into(), secret("x")).unwrap();
    assert_eq!(store.lookup("two").unwrap().unwrap().expose_secret(), "x");
}

#[test]
fn invalid_names_fail_before_environment_access() {
    let store = MemoryCredentials::with_environment(CredentialLimits::default(), |_| {
        panic!("invalid names must not query environment")
    });
    for name in ["", " ", "a=b", "a\0b", "a\nb", "\t"] {
        assert!(matches!(
            store.lookup(name),
            Err(CredentialError::InvalidName)
        ));
        assert_eq!(
            store.remember(name.into(), secret("x")),
            Err(CredentialError::InvalidName)
        );
    }
}

#[test]
fn environment_values_are_bounded_and_non_unicode_errors_do_not_retain_raw_os_values() {
    let limits = CredentialLimits {
        value_bytes: 4,
        ..CredentialLimits::default()
    };
    let store = MemoryCredentials::with_environment(limits, |name| match name {
        "WES_LARGE" => Ok(Some(secret("oversized-fixture"))),
        "WES_BLANK" => Ok(Some(secret(" \t"))),
        "WES_INVALID" => Err(CredentialError::InvalidEnvironment),
        _ => Ok(None),
    });
    assert!(matches!(
        store.lookup("large"),
        Err(CredentialError::TooLarge)
    ));
    assert!(store.lookup("blank").unwrap().is_none());
    assert!(store.lookup("missing").unwrap().is_none());
    assert!(matches!(
        store.lookup("invalid"),
        Err(CredentialError::InvalidEnvironment)
    ));
    assert!(store.names().unwrap().is_empty());
}

#[test]
fn non_breaking_spaces_are_not_silently_treated_as_empty_tokens() {
    let store = empty(CredentialLimits::default());
    for token in ["\u{a0}", "\u{2007}", "\u{202f}", "\u{85}"] {
        store.remember("key".into(), secret(token)).unwrap();
        assert_eq!(store.lookup("key").unwrap().unwrap().expose_secret(), token);
    }
    for token in ["\u{2000}", "\u{2029}", "\u{1c}", "\u{3000}"] {
        store.remember("key".into(), secret("value")).unwrap();
        store.remember("key".into(), secret(token)).unwrap();
        assert!(store.lookup("key").unwrap().is_none());
    }
}

#[test]
fn environment_names_use_locale_independent_uppercase_and_the_documented_separator_mapping() {
    for (name, expected) in [
        ("catalog-key", "WES_CATALOG_KEY"),
        ("catalog.key", "WES_CATALOG_KEY"),
        ("Mixed_Case", "WES_MIXED_CASE"),
        ("straße", "WES_STRASSE"),
        ("ölçü", "WES_ÖLÇÜ"),
    ] {
        assert_eq!(environment_variable(name), expected);
    }
}

#[test]
fn independent_threads_can_supply_and_read_credentials_without_partial_values() {
    let store = Arc::new(empty(CredentialLimits::default()));
    let threads = (0..8)
        .map(|thread| {
            let store = store.clone();
            std::thread::spawn(move || {
                let name = format!("key-{thread}");
                for revision in 0..100 {
                    let text = format!("fixture-{thread}-{revision}");
                    store.remember(name.clone(), secret(&text)).unwrap();
                    assert_eq!(store.lookup(&name).unwrap().unwrap().expose_secret(), &text);
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(store.names().unwrap().len(), 8);
}
