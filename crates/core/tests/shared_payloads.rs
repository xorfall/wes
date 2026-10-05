use std::sync::Arc;
use wes_core::Data;

#[test]
fn cloned_containers_share_immutable_payloads_without_sharing_mutations() {
    let text: Arc<str> = "x".repeat(10_000).into();
    let bytes: Arc<[u8]> = vec![42; 10_000].into();
    let original = Data::List(vec![Data::Text(text.clone()), Data::Bytes(bytes.clone())]);
    let Data::List(mut copy) = original.clone() else {
        panic!()
    };
    let (Data::Text(copied_text), Data::Bytes(copied_bytes)) = (&copy[0], &copy[1]) else {
        panic!()
    };
    assert!(Arc::ptr_eq(&text, copied_text));
    assert!(Arc::ptr_eq(&bytes, copied_bytes));
    copy[0] = Data::Text("changed".into());
    let Data::List(items) = &original else {
        panic!()
    };
    assert_eq!(items[0], Data::Text(text));
    assert_eq!(items[1], Data::Bytes(bytes));
}
