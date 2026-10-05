use std::fs;
use wes_adapters::inventory::{FileInventory, InventoryError};

#[test]
fn inventory_counts_real_metadata_fresh_without_changing_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("generation")).unwrap();
    fs::write(root.path().join("marker"), b"123").unwrap();
    fs::write(root.path().join("generation/journal"), b"12345").unwrap();
    let mut inventory = FileInventory::new();
    inventory
        .add("workspaces", root.path(), "All named histories", true, 1)
        .unwrap();
    let first = inventory.read().unwrap().remove(0);
    assert_eq!((first.files, first.bytes), (2, 8));
    assert!(first.durable);
    fs::write(root.path().join("generation/journal"), b"123456789").unwrap();
    assert_eq!(inventory.read().unwrap()[0].bytes, 12);
    assert_eq!(fs::read(root.path().join("marker")).unwrap(), b"123");
    assert!(
        inventory
            .add("missing", &root.path().join("missing"), "", false, 0)
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn inventory_uses_captured_directory_and_refuses_links_and_unconfigured_depth() {
    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("selected");
    fs::create_dir(&selected).unwrap();
    fs::write(selected.join("one"), "abc").unwrap();
    let mut inventory = FileInventory::new();
    inventory
        .add("live", &selected, "values", false, 0)
        .unwrap();
    let moved = root.path().join("moved");
    fs::rename(&selected, &moved).unwrap();
    fs::create_dir(&selected).unwrap();
    fs::write(selected.join("replacement"), "abcdef").unwrap();
    assert_eq!(inventory.read().unwrap()[0].bytes, 3);
    std::os::unix::fs::symlink(&selected, moved.join("link")).unwrap();
    assert!(matches!(inventory.read(), Err(InventoryError::Invalid)));
    fs::remove_file(moved.join("link")).unwrap();
    fs::create_dir(moved.join("nested")).unwrap();
    assert!(matches!(inventory.read(), Err(InventoryError::Invalid)));
}
