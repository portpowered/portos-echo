use super::*;

fn diagnostic(line: usize) -> String {
    format!(
        "crates/example/src/lib.rs:{line}: secret-format: Secret must not implement Debug or Display or derive Debug"
    )
}

fn scan(source: &str) -> Vec<String> {
    check("crates/example/src/lib.rs", source)
}

#[test]
fn generic_qualified_and_multiline_formatting_impls() {
    let source = "\nimpl<T: Copy>\n std::fmt::Debug\n for crate::secret::Secret<T> {}\nimpl<'a, T> core::fmt::Display for Secret<&'a T> where T: Copy {}";
    assert_eq!(scan(source), [diagnostic(2), diagnostic(5)]);
    assert_eq!(scan("impl Debug for Secret {}"), [diagnostic(1)]);
    assert_eq!(scan("impl<T> Debug for Secret<T> where T: Debug {}"), [diagnostic(1)]);
}

#[test]
fn derives_on_secret_named_tuple_and_nested_fields() {
    for body in [
        "struct Secret<T>(T);",
        "struct Holder { value: Secret<String> }",
        "pub(crate) struct Holder(pub crate::Secret<String>);",
        "struct Holder<T> where T: Copy { value: Option<echo_contracts::Secret<T>> }",
    ] {
        assert_eq!(
            scan(&format!("#[derive(Clone, core::fmt::Debug)]\n{body}")),
            [diagnostic(1)]
        );
    }
}

#[test]
fn attributes_visibility_and_crlf_preserve_derive_location() {
    let source = "// heading\r\n#[derive(\r\n Clone,\r\n Debug\r\n)]\r\n#[repr(C)]\r\npub(in crate::module) struct Holder(Secret<String>);";
    assert_eq!(scan(source), [diagnostic(2)]);
    assert_eq!(
        scan("#[derive(Clone)] #[derive(Debug)] struct Secret<T>(T);"),
        [diagnostic(1)]
    );
}

#[test]
fn unrelated_traits_types_bounds_and_following_items_are_safe() {
    let source = "impl<T: Debug> Other for Secret<T> {}\nimpl Debug for Public<Secret<String>> {}\nimpl Secret<String> {}\n#[derive(Debug)] struct Safe<T: Secret>(T);\n#[derive(Debug)] struct Unit;\nstruct Holder(Secret<String>);\n#[derive(Debug)] fn unrelated() {}\nstruct Another(Secret<String>);";
    assert_eq!(scan(source), Vec::<String>::new());
}

#[test]
fn comments_strings_characters_and_lifetimes_do_not_become_syntax() {
    let source = r####"
// impl Debug for Secret<String> {}
/* outer /* #[derive(Debug)] struct Secret; */ nested */
const TEXT: &str = "impl Debug for Secret<String> {}";
const RAW: &str = r###"#[derive(Debug)] struct Secret;"###;
const BYTE: &[u8] = br#"impl Display for Secret {}"#;
const C: &CStr = c"impl Display for Secret {}";
const CHAR: char = '\'';
const UNICODE: char = 'é';
const ESCAPE: char = '\u{1f600}';
fn borrow<'a>(value: &'a str) -> &'a str { value }
#[derive(Debug)] struct Safe { /* Secret<String> */ text: &'static str }
"####;
    assert_eq!(scan(source), Vec::<String>::new());
    assert_eq!(
        scan("/* nested /* end */ end */\nimpl Debug for Secret {}"),
        [diagnostic(2)]
    );
    assert_eq!(
        scan("const S: &str = \"escaped \\\"\";\nimpl Display for Secret {}"),
        [diagnostic(2)]
    );
}

#[test]
fn all_inline_and_external_syntax_is_scanned_without_printing_values() {
    let source = "mod inline {\n#[derive(Debug)] struct Holder(Secret<String>);\n}\nimpl std::fmt::Display for crate::Secret<String> {}";
    assert_eq!(scan(source), [diagnostic(2), diagnostic(4)]);
    assert_eq!(
        scan("#[derive(Debug)] struct Holder { value: Secret<String>, text: &'static str = \"private value\" }"),
        [diagnostic(1)]
    );
}

#[test]
fn incomplete_sources_do_not_panic_or_invent_a_violation() {
    for source in [
        "#",
        "#[derive(Debug)",
        "impl<T",
        "#[derive(Debug)]",
        "/*",
        "\"",
        "r###\"",
        "'lifetime",
        "#[derive(Debug)] struct Safe<",
        "#[derive(Debug)] struct Safe(",
    ] {
        assert_eq!(scan(source), Vec::<String>::new(), "{source}");
    }
}

#[test]
fn lexer_handles_byte_char_raw_c_strings_and_unicode_positions() {
    let source = "b'\\n' cr##\"Debug\nSecret\"## 'λ' λ\nimpl Debug for Secret {}";
    let tokens = tokens::lex(source);
    assert_eq!(
        tokens.iter().map(|token| token.text).collect::<Vec<_>>(),
        ["λ", "impl", "Debug", "for", "Secret", "{", "}"]
    );
    assert_eq!(scan(source), [diagnostic(3)]);
}
