//! Typed presentations execute the checked-in calculation sources.
use super::*;
use std::collections::VecDeque;
async fn run(w: &mut Workspace) {
    let mut work = VecDeque::from(w.start(Duration::ZERO));
    while let Some(effect) = work.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            let ticket = w.enter_ticket(ticket).unwrap().unwrap();
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            assert!(
                matches!(report.outcome, Outcome::Produced(_)),
                "{:?}",
                report.outcome
            );
            work.extend(w.complete(&run, report.outcome, Duration::ZERO));
        }
    }
}
fn field<'a>(value: &'a Data, name: &str) -> &'a Data {
    let Data::Record(r) = value else { panic!() };
    &r[name]
}
#[tokio::test]
async fn actual_typed_presentation_files_cover_table_csv_histogram_and_text() {
    let mut w = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let package = w
        .prepare_type_package(
            include_str!("../../../../examples/typed-presentations/types.yaml"),
            Span::at(0),
        )
        .unwrap();
    w.commit(package).unwrap();
    for source in [
        include_str!("../../../../examples/typed-presentations/conversions.wes"),
        include_str!("../../../../examples/typed-presentations/sample.wes"),
    ] {
        let parsed = parse(&SourceText::new("typed-presentations", source));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        for statement in parsed.script.statements {
            let p = w.prepare(&statement).unwrap();
            let Preparation::Change(p) = p else { panic!() };
            w.commit(p).unwrap();
        }
    }
    run(&mut w).await;
    let data = |name: &str| {
        w.runtime()
            .value_of(&w.resolve(name).unwrap().node)
            .unwrap()
            .data()
    };
    assert_eq!(
        data("sample_summary"),
        &Data::Text("Samples: 5 items".into())
    );
    assert_eq!(
        data("price_csv"),
        &Data::Text("\"time\",\"price\"\r\n\"first\",\"1.25\"\r\n\"second\",\"2.5\"\r\n".into())
    );
    assert_eq!(
        data("escaped_csv"),
        &Data::Text("\"message\"\r\n\"a,\"\"b\"\"\nnext\"\r\n".into())
    );
    let Data::List(bins) = field(data("distribution"), "bins") else {
        panic!()
    };
    assert_eq!(field(&bins[0], "count"), &Data::Int(2));
    assert_eq!(field(&bins[1], "count"), &Data::Int(3));
    for (name, kind) in [("price_table", "table"), ("distribution", "histogram")] {
        assert_eq!(field(data(name), "view"), &Data::Text(kind.into()));
    }
    for definition in w.templates().snapshot().values() {
        assert!(
            !definition
                .calculation
                .as_ref()
                .unwrap()
                .compiled
                .effectful()
        );
    }
}
#[test]
fn new_view_packages_are_refused_without_installing_types_or_definitions() {
    let w = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    assert!(
        w.prepare_type_package(
            "types: {MustNotInstall: {base: Text}}\nviews: {}",
            Span::at(0)
        )
        .is_err()
    );
    assert!(w.contracts().resolve("MustNotInstall").is_err());
    assert!(w.contracts().sources().is_empty());
}
