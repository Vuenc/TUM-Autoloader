use std::iter::FromIterator;
use lazy_static::lazy_static;
use reqwest::header::{self, HeaderValue};

lazy_static! {
    pub static ref DEFAULT_HEADERS: header::HeaderMap = header::HeaderMap::from_iter(vec![
        (header::USER_AGENT, HeaderValue::from_str("Mozilla/5.0 (X11; Linux x86_64; rv:100.0) Gecko/20100101 Firefox/100.0").unwrap())
    ]);
}
