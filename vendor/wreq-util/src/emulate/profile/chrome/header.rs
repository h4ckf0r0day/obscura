use super::*;

pub fn header_initializer(
    sec_ch_ua: &'static str,
    ua: &'static str,
    emulation_os: Platform,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    header_chrome_sec_ch_ua!(
        headers,
        sec_ch_ua,
        emulation_os.platform(),
        emulation_os.is_mobile()
    );
    header_chrome_ua!(headers, ua);
    header_chrome_sec_fetch!(headers);
    header_chrome_accept!(headers);
    headers
}

pub fn header_initializer_with_zstd(
    sec_ch_ua: &'static str,
    ua: &'static str,
    emulation_os: Platform,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    header_chrome_sec_ch_ua!(
        headers,
        sec_ch_ua,
        emulation_os.platform(),
        emulation_os.is_mobile()
    );
    header_chrome_ua!(headers, ua);
    header_chrome_sec_fetch!(headers);
    header_chrome_accept!(zstd, headers);
    headers
}

pub fn header_initializer_with_zstd_priority(
    sec_ch_ua: &'static str,
    ua: &'static str,
    emulation_os: Platform,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    header_chrome_sec_ch_ua!(
        headers,
        sec_ch_ua,
        emulation_os.platform(),
        emulation_os.is_mobile()
    );
    header_chrome_ua!(headers, ua);
    header_chrome_sec_fetch!(headers);
    header_chrome_accept!(zstd, headers);
    headers.insert(
        HeaderName::from_static("priority"),
        HeaderValue::from_static("u=0, i"),
    );
    headers
}
