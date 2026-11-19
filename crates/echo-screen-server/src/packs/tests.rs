//! T1 data validation and disk publication; no network or external art.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    sync::{Arc, atomic::AtomicBool},
};

const PACK: &[u8] = br#"{"format":1,"name":"template","version":"0.1.0","character":{"rest":"rest.png","states":{"idle":{"clip":{"webm":"idle.webm","mode":"loop"}}}}}"#;

pub(super) fn valid_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("pack.json", PACK.to_vec()),
        ("rest.png", b"generated-rest".to_vec()),
        ("idle.webm", b"generated-loop".to_vec()),
    ]
}

pub(super) fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

pub(super) fn archive(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (path, bytes) in files {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, path, bytes.as_slice()).unwrap();
    }
    gzip(&archive.into_inner().unwrap())
}

fn prepare(root: &std::path::Path, bytes: &[u8], digest: Option<&str>) -> Result<archive::Prepared, Error> {
    let mut compressed = tempfile::NamedTempFile::new().unwrap();
    compressed.write_all(bytes).unwrap();
    let good: String = Sha256::digest(bytes)
        .iter()
        .flat_map(|byte| {
            let digits = b"0123456789abcdef";
            [
                char::from(digits[usize::from(byte >> 4)]),
                char::from(digits[usize::from(byte & 15)]),
            ]
        })
        .collect();
    archive::prepare(
        root,
        compressed.path(),
        digest.unwrap_or(&good),
        &Arc::new(AtomicBool::new(false)),
    )
}

fn refused(bytes: &[u8], expected: Error) {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(prepare(root.path(), bytes, None).err(), Some(expected));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn pack_install_digest_precedes_parsing_and_writing() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        prepare(root.path(), b"not a gzip", Some(&"0".repeat(64))).err(),
        Some(Error::Digest)
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    refused(b"not a gzip", Error::Archive);
}

#[test]
fn pack_install_publishes_exact_bytes_deduplicates_and_retains_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let bytes = archive(&valid_files());
    let stop = AtomicBool::new(false);
    let pending = prepare(root.path(), &bytes, None).unwrap();
    assert!(!root.path().join("template@0.1.0").exists());
    assert_eq!(pending.publish(root.path(), &stop).unwrap(), "template@0.1.0");
    for (file, expected) in valid_files() {
        assert_eq!(
            std::fs::read(root.path().join("template@0.1.0").join(file)).unwrap(),
            expected
        );
    }
    assert_eq!(
        prepare(root.path(), &bytes, None)
            .unwrap()
            .publish(root.path(), &stop)
            .unwrap(),
        "template@0.1.0"
    );
    let mut changed = valid_files();
    changed[1].1 = b"changed".to_vec();
    assert_eq!(
        prepare(root.path(), &archive(&changed), None)
            .unwrap()
            .publish(root.path(), &stop),
        Err(Error::Conflict)
    );
    assert_eq!(
        std::fs::read(root.path().join("template@0.1.0/rest.png")).unwrap(),
        b"generated-rest"
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
}

// Raw headers express hostile paths/sizes that tar::Builder correctly refuses.
fn hostile(path: &[u8], kind: u8, size: u64) -> Vec<u8> {
    let mut header = tar::Header::new_ustar();
    header.as_mut_bytes()[..path.len()].copy_from_slice(path);
    header.set_entry_type(tar::EntryType::new(kind));
    header.set_mode(0o644);
    header.set_size(size);
    header.set_cksum();
    gzip(header.as_bytes())
}

#[test]
fn pack_install_rejects_absolute_dotdot_and_noncanonical_paths() {
    for path in [
        "/rest.png",
        "../rest.png",
        "images/../rest.png",
        "C:/rest.png",
        "a\\b.png",
        "a//b.png",
        "./rest.png",
    ] {
        refused(&hostile(path.as_bytes(), b'0', 0), Error::Path);
    }
}

#[test]
fn pack_install_rejects_links_devices_fifo_and_parser_extensions() {
    for kind in [b'1', b'2'] {
        refused(&hostile(b"rest.png", kind, 0), Error::Link);
    }
    for kind in [b'3', b'4', b'5', b'6', b'7', b'g', b'x', b'L', b'K', b'S'] {
        refused(&hostile(b"rest.png", kind, 0), Error::Entry);
    }
}

#[test]
fn pack_install_rejects_clip_size_expanded_size_count_duplicates_and_types() {
    refused(&hostile(b"idle.webm", b'0', CLIP_LIMIT + 1), Error::Clip);
    refused(&hostile(b"rest.png", b'0', EXPANDED_LIMIT + 1), Error::Size);
    let files: Vec<_> = (0..65).map(|n| (format!("{n}.png"), vec![])).collect();
    let refs: Vec<_> = files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
    refused(&archive(&refs), Error::Count);
    refused(
        &archive(&[("rest.png", vec![]), ("rest.png", vec![])]),
        Error::Duplicate,
    );
    refused(&hostile(b"active.html", b'0', 0), Error::Type);
}

#[test]
fn pack_install_accepts_exact_file_and_clip_caps_and_rejects_padding_bomb() {
    let root = tempfile::tempdir().unwrap();
    let mut files = valid_files();
    files[2].1 = vec![0; usize::try_from(CLIP_LIMIT).unwrap()];
    let extras: Vec<_> = (0..61).map(|n| (format!("extra-{n}.png"), vec![])).collect();
    files.extend(extras.iter().map(|(name, bytes)| (name.as_str(), bytes.clone())));
    assert_eq!(files.len(), FILE_LIMIT);
    let prepared = prepare(root.path(), &archive(&files), None).unwrap();
    assert_eq!(prepared.identity(), "template@0.1.0");
    drop(prepared);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    refused(
        &gzip(&vec![0; usize::try_from(EXPANDED_LIMIT + MIB + 1).unwrap()]),
        Error::Size,
    );
}

fn layout() -> serde_json::Value {
    serde_json::json!({
        "safe_area":{"x":0,"y":0,"width":100,"height":100},
        "slots":{"clock":{"x":10,"y":15,"color":"#ffffff","font_scale":1}},
        "stage":{"anchor":{"x":50,"y":50},"scale":1,"background":"#000000"},
        "css":"style.css"
    })
}

#[test]
fn pack_manifest_accepts_layout_only_and_validates_geometry_unknown_states_and_nulls() {
    let mut files: BTreeMap<_, _> = valid_files().into_iter().map(|(p, b)| (p.to_owned(), b)).collect();
    files.insert("style.css".into(), b".pack-clock {color:#ffffff}".to_vec());
    let pack = serde_json::json!({"format":1,"name":"template","version":"0.1.0","layout":layout()});
    files.insert("pack.json".into(), serde_json::to_vec(&pack).unwrap());
    assert_eq!(manifest::parse(&files).unwrap().name, "template");
    for (field, bad) in [
        ("safe_area", serde_json::json!({"x":90,"y":0,"width":20,"height":100})),
        (
            "stage",
            serde_json::json!({"anchor":{"x":50,"y":50},"scale":0,"background":"#000000"}),
        ),
        ("slots", serde_json::json!({"clock":{"x":101,"y":0}})),
        ("slots", serde_json::json!({"clock":{"x":0,"y":0,"font_scale":3}})),
        ("slots", serde_json::json!({"clock":null})),
    ] {
        let mut bad_pack = pack.clone();
        bad_pack["layout"][field] = bad;
        files.insert("pack.json".into(), serde_json::to_vec(&bad_pack).unwrap());
        assert_eq!(manifest::parse(&files).err(), Some(Error::Manifest));
    }
    let unknown = String::from_utf8(PACK.to_vec())
        .unwrap()
        .replace("\"idle\"", "\"unmapped\"");
    files.insert("pack.json".into(), unknown.into_bytes());
    assert_eq!(manifest::parse(&files).err(), Some(Error::Manifest));
}

#[test]
fn pack_manifest_rejects_bad_schema_roles_references_and_geometry() {
    for pack in [
        serde_json::json!({"format":2,"name":"template","version":"0.1.0","character":{"rest":"rest.png"}}),
        serde_json::json!({"format":1,"name":"../template","version":"0.1.0","character":{"rest":"rest.png"}}),
        serde_json::json!({"format":1,"name":"template","version":"0.1.0"}),
        serde_json::json!({"format":1,"name":"template","version":"0.1.0","character":{"rest":"rest.png"},"script":"active"}),
    ] {
        let mut files = valid_files();
        files[0].1 = serde_json::to_vec(&pack).unwrap();
        refused(&archive(&files), Error::Manifest);
    }
    let mut files = valid_files();
    files[0].1 = b"{invalid".to_vec();
    refused(&archive(&files), Error::Manifest);
    files = valid_files();
    files.retain(|(p, _)| *p != "rest.png");
    refused(&archive(&files), Error::Reference);
    files = valid_files();
    files[0].1 = String::from_utf8(PACK.to_vec())
        .unwrap()
        .replace("rest.png", "idle.webm")
        .into_bytes();
    refused(&archive(&files), Error::Reference);
}

#[test]
fn pack_css_accepts_only_documented_selectors_declarations_and_local_files() {
    let files = BTreeMap::from([("rest.png".into(), vec![])]);
    assert_eq!(
        css::validate(
            ".pack-clock, .pack-date { color: #ffffff; font-size: 2rem; } .pack-stage {background-image:url('rest.png'); opacity:0.8}",
            &files
        ),
        Ok(())
    );
    for value in [
        ".pack-stage {background-image:url(https://127.0.0.1/rest.png)}",
        ".pack-stage {background-image:url(//127.0.0.1/rest.png)}",
        ".pack-stage {background-image:url(data:image/png,abc)}",
        ".pack-stage {background-image:url(../rest.png)}",
        ".pack-stage {background-image:url(missing.png)}",
        "@import 'rest.css';",
        ".pack-clock {color:expression(alert(1))}",
        ".pack-clock {background-image:u/**/rl(rest.png)}",
        ".pack-clock {background-image:u\\72l(rest.png)}",
        "body {color:#ffffff}",
        ".pack-clock:hover {color:#ffffff}",
        ".pack-clock {position:fixed}",
        ".pack-clock {opacity:NaN}",
    ] {
        assert_eq!(css::validate(value, &files), Err(Error::Css), "{value}");
    }
    let mut pack = valid_files();
    pack.push(("extra.css", b"@import 'remote.css';".to_vec()));
    refused(&archive(&pack), Error::Css);
}

#[test]
fn pack_install_cancelled_preparation_and_publication_leave_no_identity() {
    let root = tempfile::tempdir().unwrap();
    let bytes = archive(&valid_files());
    let prepared = prepare(root.path(), &bytes, None).unwrap();
    assert_eq!(
        prepared.publish(root.path(), &AtomicBool::new(true)),
        Err(Error::Cancelled)
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn pack_download_requires_configured_roots_and_strict_https() {
    assert_eq!(download::client(None).await.err(), Some(Error::Ca));
    for value in [
        "http://127.0.0.1/pack",
        "https://user@127.0.0.1/pack",
        "https://127.0.0.1/#fragment",
        "https://127.0.0.1/white space",
    ] {
        assert_eq!(download::url(value).err(), Some(Error::Url));
    }
    let ca = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ca.path(), b"not certificates").unwrap();
    assert_eq!(download::client(Some(ca.path())).await.err(), Some(Error::Ca));
}
