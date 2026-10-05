use std::fs;
use wes_adapters::completion::ShellCompleter;
use wes_engine::{
    completion::{Completer, CompletionError},
    driver::CancellationToken,
};

#[cfg(unix)]
#[test]
fn programs_are_read_only_sorted_deduplicated_and_limited_to_executable_files() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    for directory in ["first", "second"] {
        fs::create_dir(root.path().join(directory)).unwrap();
    }
    for (directory, name) in [
        ("first", "zulu"),
        ("first", "echo"),
        ("second", "alpha"),
        ("second", "zulu"),
    ] {
        let path = root.path().join(directory).join(name);
        // Executing a candidate would leave evidence. Completion only inspects metadata.
        fs::write(&path, "#!/bin/sh\ntouch was-executed\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(root.path().join("first/not-executable"), "x").unwrap();
    fs::create_dir(root.path().join("first/directory")).unwrap();
    let completer = ShellCompleter::new(
        root.path().into(),
        None,
        vec!["first".into(), "second".into()],
    )
    .unwrap();
    let answer = completer
        .complete("", 0, &CancellationToken::new())
        .unwrap();
    let names: Vec<_> = answer.items.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(&names[names.len() - 2..], &["zulu", "alpha"]);
    assert_eq!(names.iter().filter(|name| **name == "echo").count(), 1);
    assert!(!names.contains(&"directory"));
    assert!(!names.contains(&"not-executable"));
    assert!(!root.path().join("was-executed").exists());
    assert_eq!(
        completer
            .complete("echo ok | pri", 13, &CancellationToken::new())
            .unwrap()
            .items[0]
            .text,
        "printf"
    );
}

#[test]
fn paths_preserve_home_prefix_quote_literals_and_use_utf8_offsets_and_utf16_order() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    for name in [
        "with space",
        "with'quote",
        "$(touch nope)",
        "~user",
        "😀",
        "\u{e000}",
        ".hidden",
    ] {
        fs::write(home.join(name), "x").unwrap();
    }
    fs::create_dir(home.join("folder")).unwrap();
    let completer = ShellCompleter::new(home.clone(), Some(home.clone()), vec![]).unwrap();
    let token = CancellationToken::new();
    let answer = completer
        .complete("echo 😀 ~/", "echo 😀 ~/".len(), &token)
        .unwrap();
    assert_eq!(answer.from, "echo 😀 ".len());
    let names: Vec<_> = answer.items.iter().map(|item| item.text.as_str()).collect();
    assert!(names.contains(&"~/'with space'"));
    assert!(names.contains(&"~/'with'\\''quote'"));
    assert!(names.contains(&"~/'$(touch nope)'"));
    assert!(names.contains(&"~/'~user'"));
    assert!(names.contains(&"~/folder/"));
    assert!(!names.contains(&"~/.hidden"));
    assert!(
        names.iter().position(|n| n.contains('😀'))
            < names.iter().position(|n| n.contains('\u{e000}'))
    );
    let literal = completer.complete("cat ~", 5, &token).unwrap();
    assert_eq!(literal.items[0].text, "'~user'");
    assert_eq!(
        completer.complete("cat .h", 6, &token).unwrap().items[0].text,
        ".hidden"
    );
    assert!(
        completer
            .complete("cat absent/", 11, &token)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        completer.complete("😀", 1, &token),
        Err(CompletionError::Invalid)
    ));
    assert!(matches!(
        completer.complete(&"x".repeat(16 * 1024 + 1), 0, &token),
        Err(CompletionError::Capacity)
    ));
    token.cancel();
    assert!(matches!(
        completer.complete("", 0, &token),
        Err(CompletionError::Cancelled)
    ));
}

#[test]
fn candidates_are_bounded_and_directory_contents_are_read_fresh() {
    let root = tempfile::tempdir().unwrap();
    let completer = ShellCompleter::new(root.path().into(), None, vec![]).unwrap();
    for i in (0..90).rev() {
        fs::write(root.path().join(format!("file{i:03}")), []).unwrap();
    }
    let token = CancellationToken::new();
    let answer = completer.complete("cat file", 8, &token).unwrap();
    assert_eq!(answer.items.len(), 60);
    assert_eq!(answer.items[0].text, "file000");
    assert_eq!(answer.items[59].text, "file059");
    fs::write(root.path().join("added"), []).unwrap();
    assert_eq!(
        completer.complete("cat add", 7, &token).unwrap().items[0].text,
        "added"
    );
}
