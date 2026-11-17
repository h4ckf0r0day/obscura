macro_rules! define_enum {
    (
        $(#[$meta:meta])*
        dispatch,
        $name:ident, $default_variant:ident,
        $const_target:ident,
        $(
            $variant:ident => ($rename:expr, $emulation_fn:path)
        ),* $(,)?
    ) => {
        $(#[$meta])*
        #[non_exhaustive]
        #[derive(Clone, Copy, Hash, Debug, PartialEq, Eq)]
        #[cfg_attr(feature = "emulation-serde", derive(Deserialize, Serialize))]
        pub enum $name {
            $(
                #[cfg_attr(feature = "emulation-serde", serde(rename = $rename))]
                $variant,
            )*
        }

        impl $name {
            pub const VARIANTS: &[$name] = &[
                $(
                    $name::$variant,
                )*
            ];

            pub fn match_emulation(self, opt: $const_target) -> wreq::Emulation {
                match self {
                    $(
                        $name::$variant => $emulation_fn(opt),
                    )*
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                $name::$default_variant
            }
        }

        #[allow(non_upper_case_globals)]
        impl $const_target {
            $(
                pub const $variant: $name = $name::$variant;
            )*
        }
    };

    (
        $(#[$meta:meta])*
        plain,
        $name:ident, $default_variant:ident,
        $(
            $variant:ident => $rename:expr
        ),* $(,)?
    ) => {
        $(#[$meta])*
        #[non_exhaustive]
        #[derive(Clone, Copy, Hash, Debug, PartialEq, Eq)]
        #[cfg_attr(feature = "emulation-serde", derive(Deserialize, Serialize))]
        pub enum $name {
            $(
                #[cfg_attr(feature = "emulation-serde", serde(rename = $rename))]
                $variant,
            )*
        }

        impl $name {
            pub const VARIANTS: &[$name] = &[
                $(
                    $name::$variant,
                )*
            ];
        }

        impl Default for $name {
            fn default() -> Self {
                $name::$default_variant
            }
        }
    };
}

macro_rules! header_chrome_sec_ch_ua {
    ($headers:expr, $ua:expr, $platform:expr, $is_mobile:expr) => {
        let mobile = if $is_mobile { "?1" } else { "?0" };
        $headers.insert("sec-ch-ua", HeaderValue::from_static($ua));
        $headers.insert("sec-ch-ua-mobile", HeaderValue::from_static(mobile));
        $headers.insert("sec-ch-ua-platform", HeaderValue::from_static($platform));
    };
}

macro_rules! header_chrome_sec_fetch {
    ($headers:expr) => {
        $headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
        $headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        $headers.insert("sec-fetch-site", HeaderValue::from_static("none"));
    };
}

macro_rules! header_chrome_ua {
    ($headers:expr, $ua:expr) => {
        $headers.insert(USER_AGENT, HeaderValue::from_static($ua));
    };
}

macro_rules! header_chrome_accept {
    ($headers:expr) => {
        $headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.9"));
        #[cfg(feature = "emulation-compression")]
        $headers.insert(
            ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br"),
        );
        $headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    };
    (zstd, $headers:expr) => {
        $headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.9"));
        #[cfg(feature = "emulation-compression")]
        $headers.insert(
            ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br, zstd"),
        );
        $headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    }
}

macro_rules! header_firefox_sec_fetch {
    ($headers:expr) => {
        $headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
        $headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        $headers.insert("sec-fetch-site", HeaderValue::from_static("none"));
    };
}

macro_rules! header_firefox_accept {
    ($headers:expr) => {
        $headers.insert(
            ACCEPT,
            HeaderValue::from_static(
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            ),
        );
        #[cfg(feature = "emulation-compression")]
        $headers.insert(
            ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br"),
        );
        $headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.5"));
    };
    (zstd, $headers:expr) => {
        $headers.insert(
            ACCEPT,
            HeaderValue::from_static(
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            ),
        );
        #[cfg(feature = "emulation-compression")]
        $headers.insert(
            ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br, zstd"),
        );
        $headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.5"));
    };
}

macro_rules! header_firefox_ua {
    ($headers:expr, $ua:expr) => {
        $headers.insert(
            HeaderName::from_static("te"),
            HeaderValue::from_static("trailers"),
        );
        $headers.insert(USER_AGENT, HeaderValue::from_static($ua));
    };
}

macro_rules! join {
    ($sep:expr, $first:expr $(, $rest:expr)*) => {
        concat!($first $(, $sep, $rest)*)
    };
}

macro_rules! standard_mod_generator {
    ($mod_name:ident, $tls_options:expr, $http2_options:expr, $headers:expr) => {
        pub(crate) mod $mod_name {
            use super::*;

            #[inline]
            pub fn emulation(emulation: Emulation) -> wreq::Emulation {
                build_emulation(emulation.http2, ($headers)(&emulation))
            }

            pub fn build_emulation(
                http2: bool,
                default_headers: Option<HeaderMap>,
            ) -> wreq::Emulation {
                build_standard_emulation(
                    stringify!($mod_name),
                    $tls_options,
                    http2.then(|| $http2_options),
                    default_headers,
                )
            }
        }
    };
    ($mod_name:ident, $build_emulation:expr, $headers:expr) => {
        pub(crate) mod $mod_name {
            use super::*;

            #[inline]
            pub fn emulation(emulation: Emulation) -> wreq::Emulation {
                $build_emulation(emulation.http2, ($headers)(&emulation))
            }
        }
    };
}

macro_rules! fixed_headers {
    ($emulation:expr, $header_initializer:ident, $ua:expr) => {
        $emulation.headers.then(|| $header_initializer($ua))
    };
}

macro_rules! platform_headers {
    (
        $emulation:expr,
        $header_initializer:ident,
        [($default_os:ident, $default_sec_ch_ua:tt, $default_ua:tt) $(, ($other_os:ident, $other_sec_ch_ua:tt, $other_ua:tt))*]
    ) => {{
        #[allow(unreachable_patterns)]
        $emulation.headers.then(|| {
            match $emulation.platform {
                $(
                    Platform::$other_os => $header_initializer(
                        $other_sec_ch_ua,
                        $other_ua,
                        $emulation.platform,
                    ),
                )*
                _ => $header_initializer(
                    $default_sec_ch_ua,
                    $default_ua,
                    Platform::$default_os,
                ),
            }
        })
    }};
}

macro_rules! firefox_platform_headers {
    ($emulation:expr, $header_initializer:ident, [($default_os:ident, $default_ua:tt) $(, ($other_os:ident, $other_ua:tt))*]) => {{
        #[allow(unreachable_patterns)]
        $emulation.headers.then(|| {
            match $emulation.platform {
                $(
                    Platform::$other_os => $header_initializer($other_ua),
                )*
                _ => $header_initializer($default_ua),
            }
        })
    }};
}

macro_rules! mod_generator {
    (
        $mod_name:ident,
        $tls_options:expr,
        $http2_options:expr,
        $header_initializer:ident,
        [($default_os:ident, $default_sec_ch_ua:tt, $default_ua:tt) $(, ($other_os:ident, $other_sec_ch_ua:tt, $other_ua:tt))*]
    ) => {
        standard_mod_generator!(
            $mod_name,
            $tls_options,
            $http2_options,
            |emulation: &Emulation| {
                platform_headers!(
                    emulation,
                    $header_initializer,
                    [($default_os, $default_sec_ch_ua, $default_ua) $(, ($other_os, $other_sec_ch_ua, $other_ua))*]
                )
            }
        );
    };
    (
        $mod_name:ident,
        $tls_options:expr,
        $http2_options:expr,
        $header_initializer:ident,
        [($default_os:ident, $default_ua:tt) $(, ($other_os:ident, $other_ua:tt))*]
    ) => {
        standard_mod_generator!(
            $mod_name,
            $tls_options,
            $http2_options,
            |emulation: &Emulation| {
                firefox_platform_headers!(
                    emulation,
                    $header_initializer,
                    [($default_os, $default_ua) $(, ($other_os, $other_ua))*]
                )
            }
        );
    };
    (
        $mod_name:ident,
        $tls_options:expr,
        $http2_options:expr,
        $header_initializer:ident,
        $ua:expr
    ) => {
        standard_mod_generator!(
            $mod_name,
            $tls_options,
            $http2_options,
            |emulation: &Emulation| fixed_headers!(emulation, $header_initializer, $ua)
        );
    };
    (
        $mod_name:ident,
        $build_emulation:expr,
        $header_initializer:ident,
        [($default_os:ident, $default_sec_ch_ua:tt, $default_ua:tt) $(, ($other_os:ident, $other_sec_ch_ua:tt, $other_ua:tt))*]
    ) => {
        standard_mod_generator!(
            $mod_name,
            $build_emulation,
            |emulation: &Emulation| {
                platform_headers!(
                    emulation,
                    $header_initializer,
                    [($default_os, $default_sec_ch_ua, $default_ua) $(, ($other_os, $other_sec_ch_ua, $other_ua))*]
                )
            }
        );
    };
    (
        $mod_name:ident,
        $build_emulation:expr,
        $header_initializer:ident,
        [($default_os:ident, $default_ua:tt) $(, ($other_os:ident, $other_ua:tt))*]
    ) => {
        standard_mod_generator!(
            $mod_name,
            $build_emulation,
            |emulation: &Emulation| {
                firefox_platform_headers!(
                    emulation,
                    $header_initializer,
                    [($default_os, $default_ua) $(, ($other_os, $other_ua))*]
                )
            }
        );
    };
    (
        $mod_name:ident,
        $build_emulation:expr,
        $header_initializer:ident,
        $ua:expr
    ) => {
        standard_mod_generator!(
            $mod_name,
            $build_emulation,
            |emulation: &Emulation| fixed_headers!(emulation, $header_initializer, $ua)
        );
    };
}
