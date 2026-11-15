//! T0 compiler witnesses: formatting a secret must fail.
#[test]
fn secret_has_no_formatting_traits() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/secret_debug.txt");
    tests.compile_fail("tests/ui/secret_display.txt");
}
