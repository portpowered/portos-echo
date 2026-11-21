use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

const INDEX: &str = include_str!("../../../web/screen/src/index.html");
const CSS: &str = include_str!("../../../web/screen/src/screen.css");

// The identity of the page this server serves: a digest of the document, both stylesheets and the script
// bundle. The document carries it in its echo-build meta. A page that reconnects to a server whose document
// names another build reloads itself, so an upgraded bridge never keeps driving the previous page.
pub(crate) fn build_id(bundle: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for part in [INDEX.as_bytes(), CSS.as_bytes(), tokens_css().as_bytes(), bundle] {
        digest.update(Sha256::digest(part));
    }
    let mut head = [0; 8];
    head.copy_from_slice(&digest.finalize()[..8]);
    format!("{:016x}", u64::from_be_bytes(head))
}

// The bundle as it is when the server starts. A development build reads the file again for every request, so
// its page identity is the one of the bundle present at startup.
pub(crate) fn startup_bundle() -> Vec<u8> {
    #[cfg(echo_screen_embedded)]
    {
        EMBEDDED.to_vec()
    }
    #[cfg(not(echo_screen_embedded))]
    {
        std::fs::read(DIST).unwrap_or_default()
    }
}

pub(crate) fn index_with_settings(settings: bool, build: &str) -> Response {
    let capabilities = if settings {
        "\n    <meta name=\"echo-settings\" content=\"v1\" />\n    <meta name=\"echo-packs\" content=\"v1\" />"
    } else {
        ""
    };
    let head = format!("<head>\n    <meta name=\"echo-build\" content=\"{build}\" />{capabilities}");
    let html = INDEX.replacen("<head>", &head, 1);
    ([("content-type", "text/html; charset=utf-8")], html).into_response()
}

pub(crate) async fn css() -> Response {
    ([("content-type", "text/css; charset=utf-8")], CSS).into_response()
}

// The vendored Port OS design tokens (third_party/portos-tokens) as CSS custom properties. Port OS switches
// scheme on `prefers-color-scheme`; the Echo panel sits in a dark room beside a dark home screen, so the dark
// scheme's values are repeated outside the media query and always apply. web/screen/tools/tokens.mjs builds the
// same sheet for the page's own tests, which compare it with this one.
const DARK_OPEN: &str = "@media (prefers-color-scheme: dark) {\n  :root {\n";
const DARK_CLOSE: &str = "  }\n}\n";

pub(crate) fn tokens_css() -> String {
    let css = portos_tokens::CSS;
    let dark = css
        .split_once(DARK_OPEN)
        .and_then(|(_, rest)| rest.split_once(DARK_CLOSE))
        .map_or("", |(dark, _)| dark);
    format!("{css}\n/* Echo: always the dark scheme. */\n:root {{\n{dark}}}\n")
}

pub(crate) async fn tokens() -> Response {
    ([("content-type", "text/css; charset=utf-8")], tokens_css()).into_response()
}

// Manrope, the one Port OS face: variable weight 200 to 800, Latin subset, SIL OFL 1.1 (OFL.txt beside it).
pub(crate) async fn font() -> Response {
    (
        [("content-type", "font/woff2")],
        include_bytes!("../../../web/screen/src/fonts/manrope-latin-wght-normal.woff2").as_slice(),
    )
        .into_response()
}

pub(crate) fn bundle(bytes: Option<Vec<u8>>) -> Response {
    match bytes {
        Some(bytes) => ([("content-type", "text/javascript; charset=utf-8")], bytes).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[cfg(echo_screen_embedded)]
const EMBEDDED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/screen.js"));
#[cfg(not(echo_screen_embedded))]
const DIST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/screen/dist/screen.js");

pub(crate) async fn js() -> Response {
    #[cfg(echo_screen_embedded)]
    {
        bundle(Some(EMBEDDED.to_vec()))
    }
    #[cfg(not(echo_screen_embedded))]
    {
        bundle(tokio::fs::read(DIST).await.ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn bundle_availability_has_no_fallback() {
        let missing = bundle(None);
        assert_eq!(missing.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(to_bytes(missing.into_body(), 1024).await.unwrap().len(), 0);
        let present = bundle(Some(b"compiled page".to_vec()));
        assert_eq!(present.status(), StatusCode::OK);
        assert_eq!(present.headers()["content-type"], "text/javascript; charset=utf-8");
        assert_eq!(to_bytes(present.into_body(), 1024).await.unwrap(), "compiled page");
    }

    #[tokio::test]
    async fn build_identity_is_stable_and_follows_the_bundle() {
        let build = build_id(b"bundle one");
        assert_eq!(build.len(), 16);
        assert!(build.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(build_id(b"bundle one"), build);
        assert_ne!(build_id(b"bundle two"), build);
        assert_ne!(build_id(b""), build);
        assert_eq!(build_id(&startup_bundle()), build_id(&startup_bundle()));
        for (settings, metas) in [(false, 1), (true, 3)] {
            let body = to_bytes(index_with_settings(settings, &build).into_body(), 4096)
                .await
                .unwrap();
            let html = std::str::from_utf8(&body).unwrap();
            let meta = format!("<head>\n    <meta name=\"echo-build\" content=\"{build}\" />\n");
            assert_eq!(html.matches(&meta).count(), 1);
            assert_eq!(html.matches("<meta name=\"echo-").count(), metas);
            assert_eq!(html.replacen(&meta, "<head>\n", 1) == INDEX, !settings);
        }
    }

    #[test]
    fn tokens_sheet_is_the_vendored_css_with_the_dark_scheme_forced() {
        use portos_tokens::color::{dark, light};
        let sheet = tokens_css();
        assert!(sheet.starts_with(portos_tokens::CSS));
        let (_, forced) = sheet.rsplit_once("\n:root {\n").unwrap();
        assert_eq!(forced.matches('{').count(), 0);
        assert!(forced.starts_with("    color-scheme: dark;\n"));
        assert!(forced.ends_with(";\n}\n"));
        for (name, color) in [
            ("background", dark::BACKGROUND),
            ("foreground", dark::FOREGROUND),
            ("border", dark::BORDER),
            ("input", dark::INPUT),
            ("ring", dark::RING),
            ("secondary", dark::SECONDARY),
            ("muted-foreground", dark::MUTED_FOREGROUND),
            ("primary", dark::PRIMARY),
            ("primary-foreground", dark::PRIMARY_FOREGROUND),
        ] {
            let line = format!("    --color-{name}: {};\n", color.hex);
            assert_eq!(forced.matches(&line).count(), 1, "{line}");
        }
        assert_eq!(dark::PRIMARY.hex, "#ffda36");
        assert_eq!(light::PRIMARY_DEFAULT, dark::PRIMARY_DEFAULT);
        assert_ne!(light::BACKGROUND, dark::BACKGROUND);
        assert!(sheet.contains("  --font-sans: Manrope, system-ui, sans-serif;\n"));
        assert_eq!(portos_tokens::font::FAMILY, "Manrope, system-ui, sans-serif");
    }

    #[tokio::test]
    async fn font_is_the_bundled_manrope_woff2() {
        let response = font().await;
        assert_eq!(response.headers()["content-type"], "font/woff2");
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        assert_eq!(bytes.len(), 24836);
        assert_eq!(&bytes[..4], b"wOF2");
    }
}
