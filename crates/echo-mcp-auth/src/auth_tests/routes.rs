//! T3 route read-back against the loopback Port OS: the nested
//! `paginationContext.nextToken`, the bounded restart after a refused
//! continuation, and the match on both deviceId and the current auth link.
use super::endpoint::{SCOPES, linked};
use super::{registration::EPOCH, *};
use crate::{EndpointExchange, EndpointTokenSource, ExchangeError, RouteScan};
use serde_json::{Value, json};

const DEVICE: &str = "synthetic-echo-device";

/// A linked owner with the endpoint audience, its exchange, and Port OS
/// knowing `DEVICE` behind `padding` unrelated routes.
async fn paired(padding: usize) -> (Mock, tempfile::TempDir, Arc<AuthClient>, EndpointExchange) {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let owner = linked(&mock, home.path(), &SCOPES).await;
    let exchange = EndpointExchange::loopback(&mock.base, Duration::from_secs(2)).unwrap();
    {
        let mut state = mock.state.lock().unwrap();
        state.endpoint.device_id = Some(DEVICE.into());
        state.endpoint.route_padding = padding;
    }
    (mock, home, owner, exchange)
}

/// The continuation each `/routes` read carried, in order. Every read repeats
/// `owner=~caller&maxResults=100`.
fn route_reads(mock: &Mock) -> Vec<Option<String>> {
    let state = mock.state.lock().unwrap();
    let reads = state.requests.iter().filter(|request| request.path == "/routes");
    reads
        .map(|read| {
            assert_eq!(read.query["owner"], "~caller");
            assert_eq!(read.query["maxResults"], "100");
            read.query.get("nextToken").cloned()
        })
        .collect()
}

/// The reads of one two-page read-back: page 1, then the cursor at `start`.
fn two_pages(start: usize) -> [Option<String>; 2] {
    [None, Some(format!("{}{start}", mock::ROUTE_TOKEN))]
}

fn scan(pages: usize, results: usize, restarts: usize) -> RouteScan {
    RouteScan {
        pages,
        results,
        routed: results,
        restarts,
    }
}

#[tokio::test(start_paused = true)]
async fn t3_route_read_back_follows_next_token_unchanged() {
    let (mock, _home, owner, exchange) = paired(150).await;
    let tokens = owner.endpoint_grant(&mock.base).unwrap();
    let found = drive(exchange.endpoint_id(&tokens, DEVICE, mock::AUTH_LINK)).await;
    assert_eq!(found.unwrap().as_deref(), Some(mock::ECHO_ENDPOINT_ID));
    let mut seen = RouteScan::default();
    let absent = drive(exchange.read_routes(&tokens, "synthetic-absent-device", mock::AUTH_LINK, &mut seen)).await;
    assert_eq!(absent.unwrap(), None);
    assert_eq!(seen, scan(2, 152, 0));
    assert_eq!(route_reads(&mock), [two_pages(100), two_pages(100)].concat());
}

/// Regression (device): a full first page of 100 unrelated routes carries only
/// `paginationContext.nextToken`; the Echo's route is on the second page.
#[tokio::test(start_paused = true)]
async fn t3_route_read_back_follows_the_nested_pagination_context() {
    let (mock, _home, owner, exchange) = paired(100).await;
    let tokens = owner.endpoint_grant(&mock.base).unwrap();
    let found = drive(exchange.endpoint_id(&tokens, DEVICE, mock::AUTH_LINK)).await;
    assert_eq!(found.unwrap().as_deref(), Some(mock::ECHO_ENDPOINT_ID));
    assert_eq!(route_reads(&mock), two_pages(100));
    // Every page carries the REST API-resource bearer, the one `/messages` takes.
    let rest = format!("Bearer {}", drive(tokens.access_token()).await.unwrap().expose());
    let state = mock.state.lock().unwrap();
    let reads = state.requests.iter().filter(|request| request.path == "/routes");
    assert!(
        reads
            .map(|read| read.headers.get("authorization"))
            .all(|bearer| bearer == Some(&rest))
    );
}

/// A modified, expired or rebound continuation gets 400: the read-back
/// restarts once from page 1 and finds the route on the new page 2.
#[tokio::test(start_paused = true)]
async fn t3_refused_continuation_restarts_once_from_page_one() {
    let (mock, _home, owner, exchange) = paired(100).await;
    mock.state.lock().unwrap().endpoint.route_token_rejections = 1;
    let tokens = owner.endpoint_grant(&mock.base).unwrap();
    let mut seen = RouteScan::default();
    let found = drive(exchange.read_routes(&tokens, DEVICE, mock::AUTH_LINK, &mut seen)).await;
    assert_eq!(found.unwrap().as_deref(), Some(mock::ECHO_ENDPOINT_ID));
    assert_eq!(seen, scan(3, 202, 1));
    assert_eq!(route_reads(&mock), [two_pages(100), two_pages(100)].concat());
}

/// A persistent 400 for continuations restarts at most once, then is a rejection.
#[tokio::test(start_paused = true)]
async fn t3_persistent_continuation_refusal_restarts_only_once() {
    let (mock, _home, owner, exchange) = paired(100).await;
    mock.state.lock().unwrap().endpoint.route_token_rejections = usize::MAX;
    let tokens = owner.endpoint_grant(&mock.base).unwrap();
    let mut seen = RouteScan::default();
    let read = drive(exchange.read_routes(&tokens, DEVICE, mock::AUTH_LINK, &mut seen)).await;
    assert_eq!(read, Err(ExchangeError::Rejected(400)));
    assert_eq!(seen, scan(2, 200, 1));
    assert_eq!(route_reads(&mock), [two_pages(100), two_pages(100)].concat());
}

/// A 400 for page 1 carried no continuation: it stays a rejection, unrestarted.
#[tokio::test(start_paused = true)]
async fn t3_first_page_refusal_is_a_rejection() {
    let (mock, _home, owner, exchange) = paired(100).await;
    mock.fault("/routes", 400, json!({}));
    let tokens = owner.endpoint_grant(&mock.base).unwrap();
    let mut seen = RouteScan::default();
    let read = drive(exchange.read_routes(&tokens, DEVICE, mock::AUTH_LINK, &mut seen)).await;
    assert_eq!(read, Err(ExchangeError::Rejected(400)));
    assert_eq!(seen, RouteScan::default());
    assert_eq!(route_reads(&mock), [None]);
}

/// A route an old pairing left for the same deviceId never matches: only the
/// current auth link's route does, and a page with only the stale one is `None`.
#[tokio::test(start_paused = true)]
async fn t3_route_matches_only_the_current_auth_link() {
    let (mock, _home, owner, exchange) = paired(0).await;
    {
        let mut state = mock.state.lock().unwrap();
        state.endpoint.stale_link = Some(mock::STALE_LINK);
        state.endpoint.route_delay = 1;
    }
    let tokens = owner.endpoint_grant(&mock.base).unwrap();
    let stale_only = drive(exchange.endpoint_id(&tokens, DEVICE, mock::AUTH_LINK)).await;
    assert_eq!(stale_only.unwrap(), None);
    let found = drive(exchange.endpoint_id(&tokens, DEVICE, mock::AUTH_LINK)).await;
    assert_eq!(found.unwrap().as_deref(), Some(mock::ECHO_ENDPOINT_ID));
    let old = drive(exchange.endpoint_id(&tokens, DEVICE, mock::STALE_LINK)).await;
    assert_eq!(old.unwrap().as_deref(), Some(mock::STALE_ENDPOINT_ID));
    assert_eq!(route_reads(&mock), [None, None, None]);
}

#[tokio::test(start_paused = true)]
async fn t3_route_pages_default_to_ten_and_cap_at_one_hundred() {
    let (mock, _home, owner, _exchange) = paired(24).await;
    mock.state.lock().unwrap().endpoint.device_id = None;
    let rest = drive(owner.endpoint_grant(&mock.base).unwrap().access_token())
        .await
        .unwrap();
    let page = |query: &'static str| {
        let request = reqwest::Client::new()
            .get(format!("{}/routes?owner=~caller{query}", mock.base))
            .bearer_auth(rest.expose())
            .send();
        async move {
            let response = request.await.unwrap();
            let status = response.status().as_u16();
            (status, response.json::<Value>().await.unwrap_or_default())
        }
    };
    let (status, first) = drive(page("")).await;
    assert_eq!(status, 200);
    assert_eq!(first["results"].as_array().unwrap().len(), 10);
    assert_eq!(
        first["paginationContext"]["nextToken"],
        format!("{}10", mock::ROUTE_TOKEN)
    );
    assert!(first.get("nextToken").is_none(), "nested, never top level");
    let (_, last) = drive(page("&maxResults=100")).await;
    assert_eq!(last["results"].as_array().unwrap().len(), 25);
    assert!(last.get("paginationContext").is_none());
    assert_eq!(drive(page("&maxResults=101")).await.0, 400);
}
