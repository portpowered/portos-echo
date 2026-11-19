use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub(crate) async fn index() -> Response {
    (
        [("content-type", "text/html; charset=utf-8")],
        include_str!("../../../web/screen/src/index.html"),
    )
        .into_response()
}

pub(crate) async fn css() -> Response {
    (
        [("content-type", "text/css; charset=utf-8")],
        include_str!("../../../web/screen/src/screen.css"),
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
}
