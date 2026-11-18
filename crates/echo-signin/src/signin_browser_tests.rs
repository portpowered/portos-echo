//! T3 facade browser lifecycle with the real callback owner and synthetic issuer.
#[path = "signin_test_support/signin_browser.rs"]
mod signin_browser;
#[path = "signin_test_support/mod.rs"]
mod support;

use crate::{OperatorCommand, SigninError, resolve_mode};
use echo_chatgpt_auth::{AuthError, AuthSource};
use echo_config::LiveMode;
use std::{fs, io::Write, time::Duration};
use support::*;

fn no_secrets(bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    for secret in [token(), REFRESH.into(), ACCOUNT.into()] {
        assert!(!text.contains(&secret));
    }
}
