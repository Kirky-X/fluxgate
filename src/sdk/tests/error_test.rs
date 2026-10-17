// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::sdk::error::FluxgateSdkError;

#[test]
fn auth_display_contains_message() {
    let err = FluxgateSdkError::Auth {
        message: "bad key".into(),
    };
    assert!(err.to_string().contains("bad key"));
}

#[test]
fn server_display_contains_status_and_message() {
    let err = FluxgateSdkError::Server {
        status: 500,
        message: "boom".into(),
    };
    let s = err.to_string();
    assert!(s.contains("500"));
    assert!(s.contains("boom"));
}

#[test]
fn from_reqwest_error_implemented() {
    // 编译时验证 From<reqwest::Error> trait impl 存在
    fn assert_from_impl<T, U>()
    where
        U: From<T>,
    {
    }
    assert_from_impl::<reqwest::Error, FluxgateSdkError>();
}
