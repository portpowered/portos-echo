//! Gateway close-code policy and the keepalive reply (live gateway, 2026-11-21).
use super::*;

#[test]
fn close_4403_stops_after_one_refreshed_retry() {
    let mut retry = Retry::default();
    assert_eq!(retry.closed(Some(CLOSE_FORBIDDEN)), None);
    assert!(retry.fresh, "the retry refreshes the token");
    assert_eq!(retry.closed(Some(CLOSE_FORBIDDEN)), Some(Status::Unavailable));
}

#[test]
fn close_4401_refreshes_and_gives_up_after_three() {
    let mut retry = Retry::default();
    for _ in 1..REFUSALS {
        assert_eq!(retry.closed(Some(CLOSE_UNAUTHORIZED)), None);
        assert!(retry.fresh);
    }
    assert_eq!(retry.closed(Some(CLOSE_UNAUTHORIZED)), Some(Status::Unavailable));
}

#[test]
fn close_4503_backs_off_with_the_same_token() {
    let mut retry = Retry::default();
    let first = retry.pause();
    assert_eq!(retry.closed(Some(CLOSE_RETRY)), None);
    assert!(!retry.fresh, "4503 keeps the token");
    let second = retry.pause();
    assert!(second > first, "the backoff keeps growing: {first:?} then {second:?}");
    assert!(second >= Duration::from_secs(2));
    assert_eq!(retry.closed(Some(CLOSE_RETRY)), None);
    assert!(retry.pause() >= Duration::from_secs(4));
}

#[test]
fn close_4000_and_4409_keep_their_meaning() {
    let mut retry = Retry::default();
    assert_eq!(retry.closed(Some(CLOSE_FORBIDDEN)), None);
    let _ = retry.pause();
    assert_eq!(retry.closed(Some(CLOSE_CYCLE)), None);
    assert!(!retry.fresh, "a cycle refreshes only a token near expiry");
    assert_eq!(retry.refusals, 0);
    assert_eq!(retry.delay, Duration::from_secs(1));
    assert_eq!(retry.closed(Some(CLOSE_REPLACED)), Some(Status::Unavailable));
}

#[test]
fn only_the_exact_pong_is_a_keepalive_reply() {
    assert!(keepalive_reply("pong"));
    for text in ["ping", "PONG", "pong ", "{\"header\":{}}"] {
        assert!(!keepalive_reply(text), "{text}");
    }
    assert_eq!(PING, "ping");
    assert_eq!(KEEPALIVE, Duration::from_secs(45));
}
