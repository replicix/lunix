mod common;

#[test]
fn smoke() {
    assert_eq!(common::hex("00 ff"), vec![0, 255]);
}
