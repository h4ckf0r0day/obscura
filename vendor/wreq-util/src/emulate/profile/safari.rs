#[macro_use]
mod http2;
#[macro_use]
mod tls;
mod header;

use header::*;
use tls::*;

use super::*;

mod_generator!(
    safari15_3,
    tls_options!(1, CIPHER_LIST_1),
    http2_options!(4),
    header_initializer_for_15,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.3 Safari/605.1.15"
);

mod_generator!(
    safari15_5,
    safari15_3::build_emulation,
    header_initializer_for_15,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15"
);

mod_generator!(
    safari15_6_1,
    tls_options!(1, CIPHER_LIST_2),
    http2_options!(4),
    header_initializer_for_15,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.6.1 Safari/605.1.15"
);

mod_generator!(
    safari16,
    safari15_6_1::build_emulation,
    header_initializer_for_16_17,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Safari/605.1.15"
);

mod_generator!(
    safari16_5,
    safari15_6_1::build_emulation,
    header_initializer_for_16_17,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.5 Safari/605.1.15"
);

mod_generator!(
    safari17_4_1,
    safari15_6_1::build_emulation,
    header_initializer_for_16_17,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4.1 Safari/605.1.15"
);

mod_generator!(
    safari_ios_16_5,
    tls_options!(1, CIPHER_LIST_2),
    http2_options!(1),
    header_initializer_for_16_17,
    "Mozilla/5.0 (iPhone; CPU iPhone OS 16_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.5 Mobile/15E148 Safari/604.1"
);

mod_generator!(
    safari17_0,
    tls_options!(1, CIPHER_LIST_2),
    http2_options!(5),
    header_initializer_for_16_17,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15"
);

mod_generator!(
    safari17_2_1,
    safari17_0::build_emulation,
    header_initializer_for_16_17,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Safari/605.1.15"
);

mod_generator!(
    safari17_5,
    safari17_0::build_emulation,
    header_initializer_for_16_17,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15"
);

mod_generator!(
    safari17_6,
    safari17_0::build_emulation,
    header_initializer_for_16_17,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15"
);

mod_generator!(
    safari_ios_17_2,
    tls_options!(1, CIPHER_LIST_2),
    http2_options!(2),
    header_initializer_for_16_17,
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1"
);

mod_generator!(
    safari_ios_17_4_1,
    safari_ios_17_2::build_emulation,
    header_initializer_for_16_17,
    "Mozilla/5.0 (iPad; CPU OS 17_4_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4.1 Mobile/15E148 Safari/604.1"
);

mod_generator!(
    safari18,
    tls_options!(1, CIPHER_LIST_2),
    http2_options!(3),
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15"
);

mod_generator!(
    safari_ipad_18,
    safari18::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"
);

mod_generator!(
    safari_ios_18_1_1,
    safari18::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1.1 Mobile/15E148 Safari/604.1"
);

mod_generator!(
    safari18_2,
    tls_options!(2, CIPHER_LIST_2, SIGALGS_LIST_2),
    http2_options!(3),
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.2 Safari/605.1.15"
);

mod_generator!(
    safari18_3,
    safari18_2::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3 Safari/605.1.15"
);

mod_generator!(
    safari18_3_1,
    safari18_2::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15"
);

mod_generator!(
    safari18_5,
    tls_options!(2, CIPHER_LIST_2, SIGALGS_LIST_2),
    http2_options!(6),
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15"
);

mod_generator!(
    safari26,
    tls_options!(3, CIPHER_LIST_3, SIGALGS_LIST_2, CURVES_2),
    http2_options!(6),
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15"
);

mod_generator!(
    safari_ipad_26,
    safari26::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (iPad; CPU OS 18_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1"
);

mod_generator!(
    safari_ios_26,
    safari26::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1"
);

mod_generator!(
    safari26_1,
    safari18_5::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.1 Safari/605.1.15"
);

mod_generator!(
    safari_ios_26_2,
    safari26::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.2 Mobile/15E148 Safari/604.1"
);

mod_generator!(
    safari26_2,
    safari18_5::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.2 Safari/605.1.15"
);

mod_generator!(
    safari26_3,
    safari18_5::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.3 Safari/605.1.15"
);

mod_generator!(
    safari26_4,
    safari18_5::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.4 Safari/605.1.15"
);

mod_generator!(
    safari_ipad_26_2,
    safari26::build_emulation,
    header_initializer_for_18,
    "Mozilla/5.0 (iPad; CPU OS 18_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.2 Mobile/15E148 Safari/604.1"
);
