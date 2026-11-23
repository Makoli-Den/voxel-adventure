//! Integration test: two packs on disk overriding one file.

// A panic in a test helper is the failure signal, as in the tests themselves.
#![allow(clippy::unwrap_used)]

use std::error::Error;
use std::io;

use vox_resources::{DirPack, PackStack, ResourceError, ResourceId};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn id(text: &str) -> ResourceId {
    ResourceId::parse(text).unwrap()
}

/// `low` below `high`.
fn stack() -> PackStack {
    let mut stack = PackStack::new();
    stack.push(DirPack::new(format!("{FIXTURES}/low")));
    stack.push(DirPack::new(format!("{FIXTURES}/high")));
    stack
}

#[test]
fn the_higher_pack_overrides_the_file() {
    assert_eq!(stack().read_to_string(&id("base:a.txt")).unwrap(), "high a");
    assert_eq!(stack().read(&id("base:a.txt")).unwrap(), b"high a");
}

#[test]
fn files_only_in_one_pack_are_not_merged_away() {
    let stack = stack();
    assert_eq!(stack.read_to_string(&id("base:b.txt")).unwrap(), "low b");
    assert_eq!(stack.read_to_string(&id("base:c.txt")).unwrap(), "high c");
}

#[test]
fn push_order_decides_priority() {
    let mut reversed = PackStack::new();
    reversed.push(DirPack::new(format!("{FIXTURES}/high")));
    reversed.push(DirPack::new(format!("{FIXTURES}/low")));
    assert_eq!(reversed.read_to_string(&id("base:a.txt")).unwrap(), "low a");
    assert_eq!(reversed.len(), 2);
}

#[test]
fn source_of_names_the_serving_pack() {
    let stack = stack();
    let high = DirPack::new(format!("{FIXTURES}/high"));
    let low = DirPack::new(format!("{FIXTURES}/low"));
    assert_eq!(stack.source_of(&id("base:a.txt")).unwrap(), &high);
    assert_eq!(stack.source_of(&id("base:b.txt")).unwrap(), &low);
    assert_eq!(stack.source_of(&id("base:c.txt")).unwrap(), &high);
}

#[test]
fn a_missing_resource_is_not_found_with_its_id() {
    let stack = stack();
    let missing = id("base:nope.txt");
    match stack.read(&missing) {
        Err(ResourceError::NotFound { id }) => assert_eq!(id, missing),
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert!(matches!(
        stack.source_of(&missing),
        Err(ResourceError::NotFound { .. })
    ));
    let message = stack.read_to_string(&missing).unwrap_err().to_string();
    assert!(message.contains("base:nope.txt"));
}

#[test]
fn non_utf8_text_is_an_io_error_but_bytes_still_read() {
    let stack = stack();
    let binary = id("base:binary.bin");
    assert_eq!(stack.read(&binary).unwrap(), [0xff, 0xfe, 0x00, 0xfd]);
    let error = stack.read_to_string(&binary).unwrap_err();
    match &error {
        ResourceError::Io { id, source } => {
            assert_eq!(id, &binary);
            assert_eq!(source.kind(), io::ErrorKind::InvalidData);
        }
        other => panic!("expected Io, got {other:?}"),
    }
    assert!(error.source().is_some());
}

#[test]
fn a_directory_where_a_file_is_expected_is_io_not_not_found() {
    let mut only_low = PackStack::new();
    only_low.push(DirPack::new(format!("{FIXTURES}/low")));
    assert!(matches!(
        only_low.read(&id("base:folder")),
        Err(ResourceError::Io { .. })
    ));
}

#[test]
fn an_error_in_a_higher_pack_is_not_hidden_by_a_lower_one() {
    // `base:folder` is a readable file in `high` but a directory in `low`; with `low` on top
    // the failure must surface instead of falling through to `high`.
    let mut low_on_top = PackStack::new();
    low_on_top.push(DirPack::new(format!("{FIXTURES}/high")));
    low_on_top.push(DirPack::new(format!("{FIXTURES}/low")));
    assert!(matches!(
        low_on_top.read(&id("base:folder")),
        Err(ResourceError::Io { .. })
    ));
    // With the order reversed the file wins and nothing fails.
    assert_eq!(
        stack().read_to_string(&id("base:folder")).unwrap(),
        "high file named folder"
    );
}
