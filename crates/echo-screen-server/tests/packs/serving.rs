//! T3 file boundary: real loopback listener, isolated local data, no media claim.
use crate::support::{Result, TOKEN, connect, head, request, server};
use std::{net::SocketAddr, path::Path};
use tokio::io::AsyncReadExt;

const MANIFEST: &str = r#"{"format":1,"name":"sample","version":"0.1.0","character":{"rest":"rest.png"}}"#;

fn fixture(home: &Path) -> Result {
    let pack = home.join("screens/sample@0.1.0");
    std::fs::create_dir_all(pack.join("media"))?;
    std::fs::write(pack.join("pack.json"), MANIFEST)?;
    for file in ["rest.png", "media/clip.webm", "media/clip.mp4", "media/still.webp"] {
        std::fs::write(pack.join(file), b"0123456789")?;
    }
    Ok(())
}

fn path(file: &str) -> String {
    format!("/screen-packs/v1/sample@0.1.0/{file}?token={TOKEN}")
}

async fn get(addr: SocketAddr, file: &str, headers: &str) -> Result<(u16, String, String)> {
    let mut stream = connect(addr, "GET", &path(file), "", headers).await?;
    let headers = head(&mut stream).await?;
    let code = headers.split_whitespace().nth(1).ok_or("missing status")?.parse()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).await?;
    assert!(headers.contains("cache-control: no-store\r\n"));
    assert!(headers.contains("referrer-policy: no-referrer\r\n"));
    Ok((code, headers, body))
}

#[tokio::test]
async fn t3_screen_pack_files_auth_exact_mimes_ranges_and_revocation() -> Result {
    let home = tempfile::tempdir()?;
    fixture(home.path())?;
    let (mut server, _states) = server().await?;
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 503);
    let _jobs = server.install_configured_packs(home.path().into(), None, vec![]);
    for query in [
        "",
        "?token=wrong",
        "?token=",
        "?token=synthetic-screen-pair&token=synthetic-screen-pair",
    ] {
        assert_eq!(
            request(
                server.bound_addr,
                "GET",
                &format!("/screen-packs/v1/sample@0.1.0/rest.png{query}"),
                "",
                ""
            )
            .await?,
            (401, String::new())
        );
    }
    for (file, mime, expected) in [
        ("pack.json", "application/json", MANIFEST),
        ("rest.png", "image/png", "0123456789"),
        ("media/still.webp", "image/webp", "0123456789"),
        ("media/clip.webm", "video/webm", "0123456789"),
        ("media/clip.mp4", "video/mp4", "0123456789"),
    ] {
        let (status, headers, body) = get(server.bound_addr, file, "").await?;
        assert_eq!((status, body.as_str()), (200, expected));
        assert!(headers.contains(&format!("content-type: {mime}\r\n")));
        assert!(headers.contains("accept-ranges: bytes\r\n"));
        assert!(headers.contains("x-content-type-options: nosniff\r\n"));
        assert!(headers.contains(&format!("content-length: {}\r\n", expected.len())));
    }
    for (range, expected, span) in [
        ("bytes=2-5", "2345", "2-5"),
        ("bytes=7-", "789", "7-9"),
        ("bytes=-3", "789", "7-9"),
        ("bytes=8-99", "89", "8-9"),
        ("bytes=-99", "0123456789", "0-9"),
    ] {
        let (status, headers, body) = get(server.bound_addr, "media/clip.mp4", &format!("Range: {range}\r\n")).await?;
        assert_eq!((status, body.as_str()), (206, expected));
        assert!(headers.contains(&format!("content-range: bytes {span}/10\r\n")));
        assert!(headers.contains(&format!("content-length: {}\r\n", expected.len())));
    }
    for range in [
        "bytes=10-",
        "bytes=5-2",
        "bytes=-0",
        "bytes=0-1,4-5",
        "words=0-1",
        "bytes=+1-2",
        "bytes=0-999999999999999999999999999999",
        "bytes=0-1\r\nRange: bytes=2-3",
    ] {
        let (status, headers, body) = get(server.bound_addr, "rest.png", &format!("Range: {range}\r\n")).await?;
        assert_eq!((status, body.as_str()), (416, ""));
        assert!(headers.contains("content-range: bytes */10\r\n"));
        assert!(headers.contains("content-length: 0\r\n"));
    }
    server.revoke_pairing().await?;
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 401);
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_screen_pack_files_revalidate_edits_and_local_identity_without_restart() -> Result {
    let home = tempfile::tempdir()?;
    fixture(home.path())?;
    let pack = home.path().join("screens/sample@0.1.0");
    let (mut server, _states) = server().await?;
    let _jobs = server.install_configured_packs(home.path().into(), None, vec![]);
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.2, "0123456789");
    std::fs::write(pack.join("rest.png"), b"edited-rest")?;
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.2, "edited-rest");
    for manifest in [
        "{",
        &MANIFEST.replace("sample", "different"),
        &MANIFEST.replace("rest.png", "missing.png"),
    ] {
        std::fs::write(pack.join("pack.json"), manifest)?;
        assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 404);
    }
    std::fs::write(pack.join("pack.json"), MANIFEST)?;
    std::fs::write(
        pack.join("unreferenced.css"),
        ".pack-stage {background-image:url(https://127.0.0.1/a.png)}",
    )?;
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 404);
    std::fs::remove_file(pack.join("unreferenced.css"))?;
    let clip = std::fs::File::create(pack.join("large.mp4"))?;
    clip.set_len(8 * 1024 * 1024 + 1)?;
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 404);
    std::fs::remove_file(pack.join("large.mp4"))?;
    for number in 0..60 {
        std::fs::write(pack.join(format!("extra{number}.png")), b"x")?;
    }
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 404);
    for number in 0..60 {
        std::fs::remove_file(pack.join(format!("extra{number}.png")))?;
    }
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.2, "edited-rest");
    std::fs::rename(&pack, home.path().join("screens/sample"))?;
    assert_eq!(
        request(
            server.bound_addr,
            "GET",
            &format!("/screen-packs/v1/sample/rest.png?token={TOKEN}"),
            "",
            ""
        )
        .await?,
        (200, "edited-rest".into())
    );
    assert_eq!(request(server.bound_addr, "GET", "/screen.css", "", "").await?.0, 200);
    server.shutdown().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn t3_screen_pack_files_reject_paths_links_and_root_aliases() -> Result {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir()?;
    fixture(home.path())?;
    let pack = home.path().join("screens/sample@0.1.0");
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("private.png"), b"never-served")?;
    let (mut server, _states) = server().await?;
    let _jobs = server.install_configured_packs(home.path().into(), None, vec![]);
    for file in [
        "../private.png",
        "/rest.png",
        "media/../rest.png",
        "%2e%2e/private.png",
        "%72est.png",
        "%ff",
        "private.html",
        "media//clip.mp4",
        "missing.png",
    ] {
        assert_eq!(get(server.bound_addr, file, "").await?.0, 404);
    }
    for target in [outside.path().join("private.png"), pack.join("rest.png")] {
        symlink(target, pack.join("link.png"))?;
        assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 404);
        std::fs::remove_file(pack.join("link.png"))?;
    }
    std::fs::hard_link(outside.path().join("private.png"), pack.join("link.png"))?;
    assert_eq!(get(server.bound_addr, "link.png", "").await?.0, 404);
    std::fs::remove_file(pack.join("link.png"))?;
    symlink(outside.path(), pack.join("escape"))?;
    assert_eq!(get(server.bound_addr, "escape/private.png", "").await?.0, 404);
    std::fs::remove_file(pack.join("escape"))?;
    let saved = home.path().join("saved");
    std::fs::rename(&pack, &saved)?;
    symlink(&saved, &pack)?;
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 404);
    std::fs::remove_file(&pack)?;
    std::fs::rename(&saved, &pack)?;
    let screens = home.path().join("screens");
    let moved = home.path().join("moved");
    std::fs::rename(&screens, &moved)?;
    symlink(&moved, &screens)?;
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.0, 404);
    assert_eq!(
        request(server.bound_addr, "GET", "/screen-packs/v1/missing/%ff", "", "")
            .await?
            .0,
        401
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_screen_pack_files_scope_styles_and_authenticate_rooted_images() -> Result {
    let home = tempfile::tempdir()?;
    fixture(home.path())?;
    let pack = home.path().join("screens/sample@0.1.0");
    std::fs::write(
        pack.join("media/look.css"),
        ".pack-clock, .pack-stage {color:#123456;background-image:url('rest.png');}",
    )?;
    let (mut server, _states) = server().await?;
    let _jobs = server.install_configured_packs(home.path().into(), None, vec![]);
    let (status, headers, body) = get(server.bound_addr, "media/look.css", "").await?;
    assert_eq!(status, 200);
    assert!(headers.contains("content-type: text/css\r\n"));
    assert_eq!(
        body,
        format!(
            ".screen-pack .pack-clock,.screen-pack .pack-stage{{color:#123456;background-image:url(\"/screen-packs/v1/sample@0.1.0/rest.png?token={TOKEN}\");}}"
        )
    );
    assert_eq!(
        get(server.bound_addr, "media/look.css", "Range: bytes=0-11\r\n")
            .await?
            .2,
        ".screen-pack"
    );
    assert_eq!(get(server.bound_addr, "rest.png", "").await?.2, "0123456789");
    server.shutdown().await?;
    Ok(())
}
