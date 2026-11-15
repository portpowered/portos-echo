use super::*;
use serde_json::json;

#[test]
fn scrub_handles_overlap_unicode_empty_and_repeated_values() {
    let serials = ["AB".into(), "ABC".into(), String::new(), "Ω".into()];
    assert_eq!(
        probe_adb::scrub("ABC/AB Ω ABCAB", &serials),
        "<serial>/<serial> <serial> <serial><serial>"
    );
    assert_eq!(
        probe_adb::scrub("unchanged", &[String::new(), String::new()]),
        "unchanged"
    );
    assert_eq!(probe_adb::scrub("", &serials), "");
    assert_eq!(probe_adb::scrub("ABC", &["serial".into(), "ABC".into()]), "<serial>");
}

#[test]
fn utc_calendar_boundaries_and_out_of_range() {
    for (seconds, expected) in [
        (0, "1970-01-01T00:00:00Z"),
        (86_399, "1970-01-01T23:59:59Z"),
        (86_400, "1970-01-02T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (978_307_199, "2000-12-31T23:59:59Z"),
        (978_307_200, "2001-01-01T00:00:00Z"),
        (4_107_542_400, "2100-03-01T00:00:00Z"),
    ] {
        assert_eq!(probe_time::timestamp(seconds).unwrap(), expected);
    }
    assert!(probe_time::timestamp(u64::MAX).is_err());
}

#[test]
fn hash_requires_one_digest_and_matching_path() {
    let digest = "abcdef0123456789abcdef0123456789";
    assert_eq!(
        probe_parse::hash(&format!("{}  /system/a\r\n", digest.to_uppercase()), "/system/a").unwrap(),
        digest
    );
    for input in [
        "",
        "abc /system/a",
        "gggggggggggggggggggggggggggggggg /system/a",
        "abcdef0123456789abcdef0123456789 /system/b",
        "abcdef0123456789abcdef0123456789 /system/a\nabcdef0123456789abcdef0123456789 /system/a",
    ] {
        assert!(probe_parse::hash(input, "/system/a").is_err(), "{input}");
    }
}

#[test]
fn display_prefers_override_and_rejects_missing_or_invalid_numbers() {
    assert_eq!(
        probe_parse::display(
            "Physical size: 1x2\r\nOverride size: 1280x800\r\n",
            "Physical density: 1\nOverride density: 213\n"
        )
        .unwrap(),
        json!({"width":1280,"height":800,"density":213})
    );
    for size in [
        "",
        "garbage",
        "Physical size: 1280",
        "Physical size: 0x800",
        "Physical size: 1x-2",
        "Physical size: 1x2\nPhysical size: 1x2",
        "Override size: 1x2\nOverride size: 1x2",
    ] {
        assert!(probe_parse::display(size, "Physical density: 213").is_err());
    }
    for density in ["Physical density: 0", "Physical density: NaN", ""] {
        assert!(probe_parse::display("Physical size: 1x2", density).is_err());
    }
}

#[test]
fn mixer_uses_exact_name_and_values_instead_of_index() {
    assert_eq!(
        probe_parse::mixer("83 INT 1 Other 7\n4 INT 2 Master TAS5805m Playback Volume 100 -5\n").unwrap(),
        json!({"name":"Master TAS5805m Playback Volume", "values":[100,-5]})
    );
    for text in [
        "",
        "83 INT 1 Other 100",
        "83 INT 1 Master TAS5805m Playback Volume Extra 1",
        "83 INT 1 Master TAS5805m Playback Volume",
        "83 INT 1 Master TAS5805m Playback Volume 1\n84 INT 1 Master TAS5805m Playback Volume 2",
    ] {
        assert!(probe_parse::mixer(text).is_err(), "{text}");
    }
}

#[test]
fn incomplete_observations_and_grammar_fail_without_processes() {
    assert!(probe_parse::manifest(&[], "1970-01-01T00:00:00Z").is_err());
    assert!(arguments(&["--adb".into(), String::new(), "--out".into(), "path".into()]).is_err());
    let mut errors = Vec::new();
    assert_eq!(run(&[], &mut std::io::sink(), &mut errors), 1);
    assert_eq!(
        String::from_utf8(errors).unwrap(),
        "echo-lab: probe requires --adb <path-to-adb> and --out <fresh-dir>\n"
    );
    let mut broken = std::io::Cursor::new([]);
    assert_eq!(run(&[], &mut std::io::sink(), &mut broken), 1);
    assert_eq!(emit("{}\n", &mut broken).unwrap_err(), "probe stdout: write failed");
    assert_eq!(
        validate_manifest("not JSON").unwrap_err(),
        "probe manifest: invalid sanitized JSON"
    );
    assert_eq!(
        validate_manifest("{}").unwrap_err(),
        "probe manifest: invalid sanitized observations"
    );
}
