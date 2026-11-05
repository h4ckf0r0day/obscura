use super::*;

pub fn header_initializer(ua: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    header_firefox_ua!(headers, ua);
    header_firefox_accept!(headers);
    header_firefox_sec_fetch!(headers);
    headers
}

pub fn header_initializer_with_zstd(ua: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    header_firefox_ua!(headers, ua);
    header_firefox_accept!(zstd, headers);
    header_firefox_sec_fetch!(headers);
    headers.insert(
        HeaderName::from_static("priority"),
        HeaderValue::from_static("u=0, i"),
    );
    headers
}
