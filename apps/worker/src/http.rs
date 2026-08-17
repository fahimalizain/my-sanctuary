//! [`api_core::HttpClient`] implementation backed by `worker::Fetch`.
//!
//! Uses the worker crate's typed `Request`/`RequestInit` APIs (which keep the
//! correct `this` binding under wasm) instead of calling the default JS fetch
//! directly — the old Go worker hit "Illegal invocation" doing that.

use api_core::HttpError;
use worker::{Fetch, Headers, Method, Request, RequestInit};

pub struct WorkerHttp;

fn http_err(err: impl std::fmt::Display) -> HttpError {
    HttpError::Message(err.to_string())
}

#[async_trait::async_trait(?Send)]
impl api_core::HttpClient for WorkerHttp {
    async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form.iter().copied())
            .finish();
        let headers = Headers::new();
        headers
            .set("Content-Type", "application/x-www-form-urlencoded")
            .map_err(http_err)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(body.into()));
        let request = Request::new_with_init(url, &init).map_err(http_err)?;
        let mut response = Fetch::Request(request).send().await.map_err(http_err)?;
        let status = response.status_code();
        let bytes = response.bytes().await.map_err(http_err)?;
        if !(200..300).contains(&status) {
            return Err(HttpError::Message(format!("POST {url} returned {status}")));
        }
        Ok(bytes)
    }

    async fn get_bearer(&self, url: &str, access_token: &str) -> Result<Vec<u8>, HttpError> {
        let headers = Headers::new();
        headers
            .set("Authorization", &format!("Bearer {access_token}"))
            .map_err(http_err)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Get).with_headers(headers);
        let request = Request::new_with_init(url, &init).map_err(http_err)?;
        let mut response = Fetch::Request(request).send().await.map_err(http_err)?;
        let status = response.status_code();
        let bytes = response.bytes().await.map_err(http_err)?;
        if !(200..300).contains(&status) {
            return Err(HttpError::Message(format!("GET {url} returned {status}")));
        }
        Ok(bytes)
    }
}
