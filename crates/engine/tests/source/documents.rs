use super::*;

const YAML: &str = "# Türkçe ĞğİıŞş ÇçÖöÜü\r\nversion: 1\ntypes:\n  EditorText: {base: Text}\n";

#[tokio::test]
async fn document_and_public_literal_share_atomic_capture_and_reader_free_replay() {
    for attached in [false, true] {
        let (mut workspace, _) = workspace();
        let command = if attached {
            ":package load source:\"\" origin:\"editor:test\"".into()
        } else {
            format!(":package load source:{}", wes_language::quote_text(YAML))
        };
        let input = input(&command)
            .with_document(attached.then(|| YAML.to_owned()))
            .unwrap();
        let SourcePreparation::Declarations(prepared) = prepare_declarations(
            input,
            workspace.draft().unwrap(),
            no_files(),
            CancellationToken::new(),
        )
        .await
        .unwrap() else {
            panic!("declarations")
        };
        assert!(
            prepared
                .diagnostics()
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error),
            "{:?}",
            prepared.diagnostics()
        );
        let record = prepared.record().unwrap().clone();
        assert_eq!(record.document.as_deref(), attached.then_some(YAML));
        assert_eq!(record.type_sources.len(), 1);
        assert_eq!(record.type_sources.values().next().unwrap(), YAML);
        assert!(
            record
                .type_sources
                .keys()
                .next()
                .unwrap()
                .starts_with("wes-text:")
        );
        prepared.commit(&mut workspace, Duration::ZERO).unwrap();
        assert!(workspace.contracts().resolve("EditorText").is_ok());
        let mut restored = wes_engine::workspace::ReplayWorkspace::new(Workspace::local(
            wes_engine::providers::LocalScope::new("fixture").unwrap(),
        ))
        .unwrap();
        let replay = restored
            .prepare(&record, CancellationToken::new())
            .await
            .unwrap();
        restored.apply(replay).unwrap();
        assert!(
            restored
                .workspace()
                .contracts()
                .resolve("EditorText")
                .is_ok()
        );
        let mut missing = record.clone();
        missing.type_sources.clear();
        assert!(
            wes_engine::workspace::ReplayWorkspace::new(Workspace::local(
                wes_engine::providers::LocalScope::new("fixture").unwrap()
            ))
            .unwrap()
            .prepare(&missing, CancellationToken::new())
            .await
            .is_err()
        );
        let mut corrupt = record;
        *corrupt.type_sources.values_mut().next().unwrap() = "types: {}".into();
        assert!(
            wes_engine::workspace::ReplayWorkspace::new(Workspace::local(
                wes_engine::providers::LocalScope::new("fixture").unwrap()
            ))
            .unwrap()
            .prepare(&corrupt, CancellationToken::new())
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn malformed_attachment_never_reads_or_installs_any_part() {
    for command in [
        ":package load path:a",
        ":package load source:x",
        ":package load source:\"already\"",
        ":package load source:\"\" source:\"\"",
        ":package load source:\"\" path:a",
        ":package load source:\"\"\n:package load source:\"\"",
        ":calc { return 1; }",
        ":env inspect source:\"\"",
        ":package load source:\"\" | :list types",
    ] {
        let (workspace, _) = workspace();
        let input = input(command).with_document(Some(YAML.into())).unwrap();
        let prepared = prepare_declarations(
            input,
            workspace.draft().unwrap(),
            no_files(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(prepared, SourcePreparation::Rejected { .. }),
            "{command}"
        );
        assert!(workspace.contracts().resolve("EditorText").is_err());
    }
    assert!(
        input(":package load source:\"\"")
            .with_document(Some("x".repeat(wes_engine::source::max_source_bytes() + 1)))
            .is_err()
    );
}

#[tokio::test]
async fn exclusive_type_inputs_and_invalid_view_leave_types_uninstalled() {
    for command in [
        ":package load".to_owned(),
        ":package load path:a source:b".into(),
        ":package load path:a origin:editor".into(),
        format!(
            ":package load source:{}",
            wes_language::quote_text(
                "types: {MustNotInstall: {base: Text}}\nviews: {Broken: {inputs: {data: Text}, render: {kind: invalid}}}"
            )
        ),
    ] {
        let (mut workspace, _) = workspace();
        let prepared = plan(&workspace, &command, no_files()).await;
        assert!(
            prepared
                .diagnostics()
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{command}"
        );
        assert!(prepared.record().is_none());
        prepared.commit(&mut workspace, Duration::ZERO).unwrap();
        assert!(workspace.contracts().resolve("MustNotInstall").is_err());
    }
}

#[tokio::test]
async fn captured_editor_archive_failure_blocks_type_installation() {
    struct Refuse;
    impl TypeSourceReader for Refuse {
        fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
            panic!("no file")
        }
        fn retain_text(&self, _: &str, _: &str) -> Result<(), TypeSourceError> {
            Err(TypeSourceError::Persistence)
        }
    }
    let (workspace, _) = workspace();
    let SourcePreparation::Declarations(prepared) = prepare_declarations(
        input(":package load source:\"\"")
            .with_document(Some(YAML.into()))
            .unwrap(),
        workspace.draft().unwrap(),
        TypeSourceCapture::live(Arc::new(Refuse)),
        CancellationToken::new(),
    )
    .await
    .unwrap() else {
        panic!("declarations")
    };
    assert!(prepared.record().is_none());
    assert!(
        prepared
            .diagnostics()
            .diagnostics
            .iter()
            .any(|d| d.message.contains("could not be saved"))
    );
}
