use std::sync::Arc;
use wes_core::{Data, IterMode, IterValue, Provenance, Shape, Value};
use wes_engine::{
    driver::CancellationToken,
    iteration::{CursorItem, SourceCursor},
};
fn plan(text: &str, mode: IterMode, arg: Option<&str>) -> Arc<IterValue> {
    Arc::new(
        IterValue::new(
            Value::new(
                Shape::Unknown,
                Data::Text(text.into()),
                Provenance::default(),
            )
            .unwrap(),
            mode,
            arg.map(str::to_owned),
            vec![],
        )
        .unwrap(),
    )
}
fn read(c: &mut SourceCursor) -> CursorItem {
    c.next_raw(&CancellationToken::new(), &mut |_| Ok(()))
        .unwrap()
}
#[test]
fn independent_cursors_empty_lines_unicode_and_terminal_none_are_distinct() {
    let p = plan("a\r\n\nb\n", IterMode::Lines, None);
    let mut a = SourceCursor::new(p.clone()).unwrap();
    let mut b = SourceCursor::new(p).unwrap();
    assert!(matches!(read(&mut a),CursorItem::Item{data:Data::Text(s),..}if s=="a".into()));
    assert!(matches!(read(&mut a),CursorItem::Item{data:Data::Text(s),..}if s.is_empty()));
    assert!(matches!(read(&mut b),CursorItem::Item{data:Data::Text(s),..}if s=="a".into()));
    assert!(matches!(read(&mut a),CursorItem::Item{data:Data::Text(s),..}if s=="b".into()));
    assert_eq!(read(&mut a), CursorItem::End);
    assert_eq!(read(&mut a), CursorItem::End);
    let mut c = SourceCursor::new(plan("😀a", IterMode::Chars, None)).unwrap();
    assert!(matches!(read(&mut c),CursorItem::Item{data:Data::Text(s),..}if s=="😀".into()));
    let p = IterValue::new(
        Value::new(
            Shape::Unknown,
            Data::List(vec![Data::Option(None)]),
            Provenance::default(),
        )
        .unwrap(),
        IterMode::Items,
        None,
        vec![],
    )
    .unwrap();
    let mut c = SourceCursor::new(Arc::new(p)).unwrap();
    assert!(matches!(
        read(&mut c),
        CursorItem::Item {
            data: Data::Option(None),
            ..
        }
    ));
    assert_eq!(read(&mut c), CursorItem::End);
    assert_eq!(read(&mut c), CursorItem::End);
}
#[test]
fn empty_regex_advances_on_unicode_boundaries_and_cancel_is_terminal() {
    let mut c = SourceCursor::new(plan("😀a", IterMode::Matches, Some(""))).unwrap();
    for _ in 0..3 {
        assert!(matches!(read(&mut c),CursorItem::Item{data:Data::Text(s),..}if s.is_empty()));
    }
    assert_eq!(read(&mut c), CursorItem::End);
    let mut c = SourceCursor::new(plan("abc", IterMode::Lines, None)).unwrap();
    let token = CancellationToken::new();
    token.cancel();
    assert!(c.next_raw(&token, &mut |_| Ok(())).unwrap_err().cancelled);
    assert!(
        c.next_raw(&CancellationToken::new(), &mut |_| Ok(()))
            .unwrap_err()
            .cancelled
    );
}

#[test]
fn shared_compiled_regex_keeps_each_cursor_position_and_cancellation_independent() {
    let p = plan("a a", IterMode::Matches, Some("a"));
    let mut a = SourceCursor::new(p.clone()).unwrap();
    let mut b = SourceCursor::new(p).unwrap();
    assert_eq!(read(&mut a), read(&mut b));
    let token = CancellationToken::new();
    token.cancel();
    assert!(a.next_raw(&token, &mut |_| Ok(())).is_err());
    assert!(matches!(read(&mut b), CursorItem::Item { index: 1, .. }));
    assert_eq!(read(&mut b), CursorItem::End);
}
