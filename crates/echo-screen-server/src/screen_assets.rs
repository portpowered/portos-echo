use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub(crate) fn index_with_settings(settings: bool) -> Response {
    let html = include_str!("../../../web/screen/src/index.html");
    let html = if settings {
        html.replacen(
            "<head>",
            "<head>\n    <meta name=\"echo-settings\" content=\"v1\" />\n    <meta name=\"echo-packs\" content=\"v1\" />",
            1,
        )
    } else {
        html.to_owned()
    };
    ([("content-type", "text/html; charset=utf-8")], html).into_response()
}

pub(crate) async fn css() -> Response {
    (
        [("content-type", "text/css; charset=utf-8")],
        include_str!("../../../web/screen/src/screen.css"),
    )
        .into_response()
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

pub(crate) async fn js() -> Response {
    #[cfg(echo_screen_embedded)]
    {
        bundle(Some(include_bytes!(concat!(env!("OUT_DIR"), "/screen.js")).to_vec()))
    }
    #[cfg(not(echo_screen_embedded))]
    {
        bundle(
            tokio::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/screen/dist/screen.js"))
                .await
                .ok(),
        )
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
