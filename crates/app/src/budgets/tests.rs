use super::*;
use serde_json::json;
#[test]
fn durable_compare_and_swap_keeps_active_policy_and_unknown_values_fail() {
    let home = tempfile::tempdir().unwrap();
    let first = Store::for_user_home(home.path());
    let second = Store::for_user_home(home.path());
    assert_eq!(first.read().unwrap().revision, 0);
    let saved = first
        .save(Change {
            revision: 0,
            values: [
                ("execution.operations".into(), 8),
                ("history.window.entries".into(), 20),
            ]
            .into(),
        })
        .unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(second.read().unwrap().values, saved.values);
    assert_eq!(wes_budgets::get("execution.operations"), 4);
    let view = snapshot(&saved);
    let operation = view["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "execution.operations")
        .unwrap();
    assert_eq!(operation["active"], 4);
    assert_eq!(operation["saved"], 8);
    assert!(matches!(
        second.save(Change {
            revision: 0,
            values: [("execution.operations".into(), 16)].into()
        }),
        Err(SaveError::Conflict)
    ));
    for values in [
        [("unknown".into(), 1)].into(),
        [("execution.operations".into(), 0)].into(),
        [("execution.operations".into(), 1025)].into(),
    ] {
        assert!(matches!(
            first.save(Change {
                revision: 1,
                values
            }),
            Err(SaveError::Invalid(_))
        ));
        assert_eq!(first.read().unwrap().revision, 1);
    }
    first
        .save(Change {
            revision: 1,
            values: [("execution.operations".into(), 4)].into(),
        })
        .unwrap();
    assert!(
        !first
            .read()
            .unwrap()
            .values
            .contains_key("execution.operations")
    );
    assert!(
        second
            .read()
            .unwrap()
            .values
            .contains_key("history.window.entries")
    );
}
#[test]
fn malformed_and_duplicate_profiles_are_preserved_and_refused() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("settings");
    crate::data_home::private_directory(&dir).unwrap();
    let store = Store::new(dir.clone());
    for bytes in [b"{\"version\":1,\"revision\":0,\"values\":{\"execution.operations\":4,\"execution.operations\":8}}".to_vec(), serde_json::to_vec(&json!({"version":1,"revision":0,"values":{"unknown":1}})).unwrap(), vec![b' '; PROFILE_BYTES+1]] {
        std::fs::write(dir.join("limits.json"), &bytes).unwrap(); assert!(store.read().is_err());
        assert!(store.save(Change{revision:0,values:BTreeMap::new()}).is_err()); assert_eq!(std::fs::read(dir.join("limits.json")).unwrap(),bytes);
    }
    for source in [
        "{\"revision\":0,\"values\":{\"execution.operations\":1.5}}",
        "{\"revision\":0,\"values\":{\"execution.operations\":4},\"extra\":true}",
    ] {
        assert!(serde_json::from_str::<Change>(source).is_err());
    }
}
#[cfg(unix)]
#[test]
fn symlink_profile_and_lock_never_follow_external_files() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let outside = home.path().join("outside");
    std::fs::write(&outside, "preserved").unwrap();
    let dir = home.path().join("settings");
    crate::data_home::private_directory(&dir).unwrap();
    let store = Store::new(dir.clone());
    symlink(&outside, dir.join("limits.json")).unwrap();
    assert!(store.read().is_err());
    assert!(
        store
            .save(Change {
                revision: 0,
                values: BTreeMap::new()
            })
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "preserved");
    std::fs::remove_file(dir.join("limits.json")).unwrap();
    std::fs::remove_file(dir.join(".limits.lock")).unwrap();
    symlink(&outside, dir.join(".limits.lock")).unwrap();
    assert!(
        store
            .save(Change {
                revision: 0,
                values: BTreeMap::new()
            })
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "preserved");
}
