#[macro_use]
mod http2;
#[macro_use]
mod tls;
mod header;

use header::*;
use tls::*;

use super::*;

mod_generator!(
    v100,
    tls_options!(1),
    http2_options!(1),
    header_initializer,
    [
        (
            MacOS,
            r#""Not A;Brand";v="99", "Chromium";v="100", "Google Chrome";v="100""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/100.0.4896.75 Safari/537.36"
        ),
        (
            Linux,
            r#""Not A;Brand";v="99", "Chromium";v="100", "Google Chrome";v="100""#,
            "Mozilla/5.0 (X11; U; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/100.0.4896.75 Safari/537.36"
        ),
        (
            Android,
            r#""Not A;Brand";v="99", "Chromium";v="100", "Google Chrome";v="100""#,
            "Mozilla/5.0 (X11; U; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/100.0.4896.75 Safari/537.36"
        ),
        (
            Windows,
            r#""Not A;Brand";v="99", "Chromium";v="100", "Google Chrome";v="100""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/100.0.4896.75 Safari/537.36"
        ),
        (
            IOS,
            r#""Not A;Brand";v="99", "Chromium";v="100", "Google Chrome";v="100""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 15_8 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/100.0.4896.85 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v101,
    v100::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Google Chrome";v="101""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/101.0.4951.67 Safari/537.36"
        ),
        (
            Linux,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Google Chrome";v="101""#,
            "Mozilla/5.0 (X11; U; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/101.0.4951.67 Safari/537.36"
        ),
        (
            Android,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Google Chrome";v="101""#,
            "Mozilla/5.0 (X11; U; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/101.0.4951.67 Safari/537.36"
        ),
        (
            Windows,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Google Chrome";v="101""#,
            "Mozilla/5.0 (X11; U; Windows x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/101.0.4951.67 Safari/537.36"
        ),
        (
            IOS,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Google Chrome";v="101""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/101.0.4951.58 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    edge101,
    v100::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Microsoft Edge";v="101""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/101.0.4951.64 Safari/537.36 Edg/101.0.1210.47"
        ),
        (
            Android,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Microsoft Edge";v="101""#,
            "Mozilla/5.0 (Linux; Android 10; ONEPLUS A6003) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/101.0.4951.41 Safari/537.36 Edg/101.0.1210.31"
        ),
        (
            Windows,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Microsoft Edge";v="101""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/101.0.4951.64 Safari/537.36 Edg/101.0.1210.53"
        ),
        // This shouldn't exist, edge was never meant to be on linux,
        // but I found some UAs in myip.ms (same for 122, 127 and 131)
        (
            Linux,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Microsoft Edge";v="101""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/101.0.4951.64 Safari/537.36 Edg/101.0.1210.53"
        ),
        (
            IOS,
            r#""Not A;Brand";v="99", "Chromium";v="101", "Microsoft Edge";v="101""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/101.0.4951.64 Mobile Safari/537.36 Edg/101.0.1210.53"
        )
    ]
);

mod_generator!(
    v104,
    v100::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Chromium";v="104", " Not A;Brand";v="99", "Google Chrome";v="104""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/104.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="104", " Not A;Brand";v="99", "Google Chrome";v="104""#,
            "Mozilla/5.0 (X11; U; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/104.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="104", " Not A;Brand";v="99", "Google Chrome";v="104""#,
            "Mozilla/5.0 (X11; U; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/104.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="104", " Not A;Brand";v="99", "Google Chrome";v="104""#,
            "Mozilla/5.0 (X11; U; Windows x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/104.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="104", " Not A;Brand";v="99", "Google Chrome";v="104""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/104.0.5112.99 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v105,
    tls_options!(2),
    http2_options!(1),
    header_initializer,
    [
        (
            MacOS,
            r#""Google Chrome";v="105", "Not)A;Brand";v="8", "Chromium";v="105""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/105.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="105", "Not)A;Brand";v="8", "Chromium";v="105""#,
            "Mozilla/5.0 (X11; U; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/105.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="105", "Not)A;Brand";v="8", "Chromium";v="105""#,
            "Mozilla/5.0 (X11; U; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/105.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="105", "Not)A;Brand";v="8", "Chromium";v="105""#,
            "Mozilla/5.0 (X11; U; Windows x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/105.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="105", "Not)A;Brand";v="8", "Chromium";v="105""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/105.0.5195.100 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v106,
    tls_options!(3),
    http2_options!(2),
    header_initializer,
    [
        (
            MacOS,
            r#""Chromium";v="106", "Google Chrome";v="106", "Not;A=Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="106", "Google Chrome";v="106", "Not;A=Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="106", "Google Chrome";v="106", "Not;A=Brand";v="99""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="106", "Google Chrome";v="106", "Not;A=Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/106.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="106", "Google Chrome";v="106", "Not;A=Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/106.0.5249.92 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v107,
    v106::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Google Chrome";v="107", "Chromium";v="107", "Not=A?Brand";v="24""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/107.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="107", "Chromium";v="107", "Not=A?Brand";v="24""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/107.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="107", "Chromium";v="107", "Not=A?Brand";v="24""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/107.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="107", "Chromium";v="107", "Not=A?Brand";v="24""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/107.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="107", "Chromium";v="107", "Not=A?Brand";v="24""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 15_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/107.0.5304.66 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v108,
    v106::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Not?A_Brand";v="8", "Chromium";v="108", "Google Chrome";v="108""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/108.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not?A_Brand";v="8", "Chromium";v="108", "Google Chrome";v="108""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/108.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not?A_Brand";v="8", "Chromium";v="108", "Google Chrome";v="108""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/108.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Not?A_Brand";v="8", "Chromium";v="108", "Google Chrome";v="108""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/108.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not?A_Brand";v="8", "Chromium";v="108", "Google Chrome";v="108""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/108.0.5359.112 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v109,
    v106::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Not_A Brand";v="99", "Google Chrome";v="109", "Chromium";v="109""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/109.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not_A Brand";v="99", "Google Chrome";v="109", "Chromium";v="109""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/109.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not_A Brand";v="99", "Google Chrome";v="109", "Chromium";v="109""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/109.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Not_A Brand";v="99", "Google Chrome";v="109", "Chromium";v="109""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/109.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not_A Brand";v="99", "Google Chrome";v="109", "Chromium";v="109""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/109.0.5414.112 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v110,
    v100::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Chromium";v="110", "Not A(Brand";v="24", "Google Chrome";v="110""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="110", "Not A(Brand";v="24", "Google Chrome";v="110""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="110", "Not A(Brand";v="24", "Google Chrome";v="110""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="110", "Not A(Brand";v="24", "Google Chrome";v="110""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="110", "Not A(Brand";v="24", "Google Chrome";v="110""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/110.0.5481.104 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v114,
    v106::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Not.A/Brand";v="8", "Chromium";v="114", "Google Chrome";v="114""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not.A/Brand";v="8", "Chromium";v="114", "Google Chrome";v="114""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not.A/Brand";v="8", "Chromium";v="114", "Google Chrome";v="114""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Not.A/Brand";v="8", "Chromium";v="114", "Google Chrome";v="114""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36"
        )
    ]
);

mod_generator!(
    v116,
    tls_options!(4),
    http2_options!(2),
    header_initializer,
    [
        (
            MacOS,
            r#""Chromium";v="116", "Not)A;Brand";v="24", "Google Chrome";v="116""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="116", "Not)A;Brand";v="24", "Google Chrome";v="116""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="116", "Not)A;Brand";v="24", "Google Chrome";v="116""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="116", "Not)A;Brand";v="24", "Google Chrome";v="116""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="116", "Not)A;Brand";v="24", "Google Chrome";v="116""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/116.0.5845.103 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v117,
    tls_options!(5),
    http2_options!(3),
    header_initializer,
    [
        (
            MacOS,
            r#""Google Chrome";v="117", "Not;A=Brand";v="8", "Chromium";v="117""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="117", "Not;A=Brand";v="8", "Chromium";v="117""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="117", "Not;A=Brand";v="8", "Chromium";v="117""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="117", "Not;A=Brand";v="8", "Chromium";v="117""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="117", "Not;A=Brand";v="8", "Chromium";v="117""#,
            "Mozilla/5.0 (iPad; CPU OS 16_0 like Mac OS X) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36"
        )
    ]
);

mod_generator!(
    v120,
    v117::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/120.0.6099.119 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    edge122,
    v117::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Chromium";v="122", "Not(A:Brand";v="24", "Microsoft Edge";v="122""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36 Edg/122.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="122", "Not(A:Brand";v="24", "Microsoft Edge";v="122""#,
            "Mozilla/5.0 (Linux; Android 10; Pixel 3 XL) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.6268.219 Safari/537.36 Edg/122.0.2238.82"
        ),
        (
            Windows,
            r#""Chromium";v="122", "Not(A:Brand";v="24", "Microsoft Edge";v="122""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36 Edg/122.0.0.0"
        ),
        // This shouldn't exist, edge was never meant to be on linux
        (
            Linux,
            r#""Chromium";v="122", "Not(A:Brand";v="24", "Microsoft Edge";v="122""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36 Edg/122.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="122", "Not(A:Brand";v="24", "Microsoft Edge";v="122""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36 Edg/122.0.0.0"
        )
    ]
);

mod_generator!(
    v123,
    v117::build_emulation,
    header_initializer_with_zstd,
    [
        (
            MacOS,
            r#""Google Chrome";v="123", "Not:A-Brand";v="8", "Chromium";v="123""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="123", "Not:A-Brand";v="8", "Chromium";v="123""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="123", "Not:A-Brand";v="8", "Chromium";v="123""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="123", "Not:A-Brand";v="8", "Chromium";v="123""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36"
        )
    ]
);

mod_generator!(
    v118,
    tls_options!(4),
    http2_options!(3),
    header_initializer,
    [
        (
            MacOS,
            r#""Chromium";v="118", "Google Chrome";v="118", "Not=A?Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="118", "Google Chrome";v="118", "Not=A?Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="118", "Google Chrome";v="118", "Not=A?Brand";v="99""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="118", "Google Chrome";v="118", "Not=A?Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="118", "Google Chrome";v="118", "Not=A?Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/118.0.5993.92 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v119,
    v118::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Google Chrome";v="119", "Chromium";v="119", "Not?A_Brand";v="24""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="119", "Chromium";v="119", "Not?A_Brand";v="24""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="119", "Chromium";v="119", "Not?A_Brand";v="24""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="119", "Chromium";v="119", "Not?A_Brand";v="24""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="119", "Chromium";v="119", "Not?A_Brand";v="24""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/119.0.6045.109 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v124,
    tls_options!(6, CURVES_2),
    http2_options!(3),
    header_initializer_with_zstd,
    [
        (
            MacOS,
            r#""Chromium";v="124", "Google Chrome";v="124", "Not-A.Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="124", "Google Chrome";v="124", "Not-A.Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="124", "Google Chrome";v="124", "Not-A.Brand";v="99""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="124", "Google Chrome";v="124", "Not-A.Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="124", "Google Chrome";v="124", "Not-A.Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/124.0.6312.52 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v126,
    v124::build_emulation,
    header_initializer_with_zstd,
    [
        (
            MacOS,
            r#""Not/A)Brand";v="8", "Chromium";v="126", "Google Chrome";v="126""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not/A)Brand";v="8", "Chromium";v="126", "Google Chrome";v="126""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not/A)Brand";v="8", "Chromium";v="126", "Google Chrome";v="126""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Not/A)Brand";v="8", "Chromium";v="126", "Google Chrome";v="126""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not/A)Brand";v="8", "Chromium";v="126", "Google Chrome";v="126""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/126.0.6478.153 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v127,
    v124::build_emulation,
    header_initializer_with_zstd,
    [
        (
            MacOS,
            r#""Not)A;Brand";v="99", "Google Chrome";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not)A;Brand";v="99", "Google Chrome";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not)A;Brand";v="99", "Google Chrome";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Not)A;Brand";v="99", "Google Chrome";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not)A;Brand";v="99", "Google Chrome";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/127.0.6533.77 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    edge127,
    v124::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Not)A;Brand";v="99", "Microsoft Edge";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36 Edg/127.0.0.0"
        ),
        (
            Android,
            r#""Not)A;Brand";v="99", "Microsoft Edge";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.6332.205 Safari/537.36 Edg/127.0.2322.67"
        ),
        (
            Windows,
            r#""Not)A;Brand";v="99", "Microsoft Edge";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36 Edg/127.0.0.0"
        ),
        // This shouldn't exist, edge was never meant to be on linux
        (
            Linux,
            r#""Not)A;Brand";v="99", "Microsoft Edge";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36 Edg/127.0.0.0"
        ),
        (
            IOS,
            r#""Not)A;Brand";v="99", "Microsoft Edge";v="127", "Chromium";v="127""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36 Edg/127.0.0.0"
        )
    ]
);

mod_generator!(
    v128,
    v124::build_emulation,
    header_initializer,
    [
        (
            MacOS,
            r#""Chromium";v="128", "Not;A=Brand";v="24", "Google Chrome";v="128""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="128", "Not;A=Brand";v="24", "Google Chrome";v="128""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="128", "Not;A=Brand";v="24", "Google Chrome";v="128""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="128", "Not;A=Brand";v="24", "Google Chrome";v="128""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="128", "Not;A=Brand";v="24", "Google Chrome";v="128""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/128.0.6613.98 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v129,
    v124::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Google Chrome";v="129", "Not=A?Brand";v="8", "Chromium";v="129""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="129", "Not=A?Brand";v="8", "Chromium";v="129""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="129", "Not=A?Brand";v="8", "Chromium";v="129""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="129", "Not=A?Brand";v="8", "Chromium";v="129""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="129", "Not=A?Brand";v="8", "Chromium";v="129""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/129.0.6668.46 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v130,
    v124::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="130", "Google Chrome";v="130", "Not?A_Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="130", "Google Chrome";v="130", "Not?A_Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="130", "Google Chrome";v="130", "Not?A_Brand";v="99""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="130", "Google Chrome";v="130", "Not?A_Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="130", "Google Chrome";v="130", "Not?A_Brand";v="99""#,
            "Mozilla/5.0 (iPad; CPU OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/130.0.6723.90 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v131,
    tls_options!(6, CURVES_3),
    http2_options!(3),
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Google Chrome";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (Linux: Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/131.0.6778.134 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    edge131,
    v131::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Microsoft Edge";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0"
        ),
        (
            Android,
            r#""Microsoft Edge";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (Linux; Android 10; HD1913) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.6778.200 Mobile Safari/537.36 EdgA/131.0.2903.87"
        ),
        (
            Windows,
            r#""Microsoft Edge";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0"
        ),
        // This shouldn't exist, edge was never meant to be on linux
        (
            Linux,
            r#""Microsoft Edge";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0"
        ),
        (
            IOS,
            r#""Microsoft Edge";v="131", "Chromium";v="131", "Not_A Brand";v="24""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0"
        )
    ]
);

mod_generator!(
    v132,
    tls_options!(7, CURVES_3),
    http2_options!(3),
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Not A(Brand";v="8", "Chromium";v="132", "Google Chrome";v="132""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not A(Brand";v="8", "Chromium";v="132", "Google Chrome";v="132""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not A(Brand";v="8", "Chromium";v="132", "Google Chrome";v="132""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Not A(Brand";v="8", "Chromium";v="132", "Google Chrome";v="132""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not A(Brand";v="8", "Chromium";v="132", "Google Chrome";v="132""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/132.0.6834.78 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v133,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Not(A:Brand";v="99", "Google Chrome";v="133", "Chromium";v="133""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not(A:Brand";v="99", "Google Chrome";v="133", "Chromium";v="133""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not(A:Brand";v="99", "Google Chrome";v="133", "Chromium";v="133""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Not(A:Brand";v="99", "Google Chrome";v="133", "Chromium";v="133""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not(A:Brand";v="99", "Google Chrome";v="133", "Chromium";v="133""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/133.0.6943.33 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v134,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Google Chrome";v="134""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Google Chrome";v="134""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Google Chrome";v="134""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Google Chrome";v="134""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Google Chrome";v="134""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/134.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    edge134,
    v131::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Microsoft Edge";v="134"#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Microsoft Edge";v="134"#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Microsoft Edge";v="134"#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Microsoft Edge";v="134"#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="134", "Not:A-Brand";v="24", "Microsoft Edge";v="134"#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/134.0.0.0"
        )
    ]
);

mod_generator!(
    edge135,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="135", "Not:A-Brand";v="24", "Microsoft Edge";v="135"#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="135", "Not:A-Brand";v="24", "Microsoft Edge";v="135"#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="135", "Not:A-Brand";v="24", "Microsoft Edge";v="135"#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="135", "Not:A-Brand";v="24", "Microsoft Edge";v="135"#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="135", "Not:A-Brand";v="24", "Microsoft Edge";v="135"#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/135.0.0.0"
        )
    ]
);

mod_generator!(
    edge136,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="136", "Not:A-Brand";v="24", "Microsoft Edge";v="136"#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="136", "Not:A-Brand";v="24", "Microsoft Edge";v="136"#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="136", "Not:A-Brand";v="24", "Microsoft Edge";v="136"#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="136", "Not:A-Brand";v="24", "Microsoft Edge";v="136"#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="136", "Not:A-Brand";v="24", "Microsoft Edge";v="136"#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/136.0.0.0"
        )
    ]
);

mod_generator!(
    edge137,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="137", "Not:A-Brand";v="24", "Microsoft Edge";v="137"#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="137", "Not:A-Brand";v="24", "Microsoft Edge";v="137"#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="137", "Not:A-Brand";v="24", "Microsoft Edge";v="137"#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="137", "Not:A-Brand";v="24", "Microsoft Edge";v="137"#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="137", "Not:A-Brand";v="24", "Microsoft Edge";v="137"#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/137.0.0.0"
        )
    ]
);

mod_generator!(
    edge138,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="138", "Not=A?Brand";v="24", "Microsoft Edge";v="138"#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="138", "Not=A?Brand";v="24", "Microsoft Edge";v="138"#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="138", "Not=A?Brand";v="24", "Microsoft Edge";v="138"#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="138", "Not=A?Brand";v="24", "Microsoft Edge";v="138"#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="138", "Not=A?Brand";v="24", "Microsoft Edge";v="138"#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/138.0.0.0"
        )
    ]
);

mod_generator!(
    edge139,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="139", "Not=A?Brand";v="24", "Microsoft Edge";v="139"#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="139", "Not=A?Brand";v="24", "Microsoft Edge";v="139"#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="139", "Not=A?Brand";v="24", "Microsoft Edge";v="139"#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="139", "Not=A?Brand";v="24", "Microsoft Edge";v="139"#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="139", "Not=A?Brand";v="24", "Microsoft Edge";v="139"#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/139.0.0.0"
        )
    ]
);

mod_generator!(
    edge140,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Microsoft Edge";v="140"#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Microsoft Edge";v="140"#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Microsoft Edge";v="140"#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Microsoft Edge";v="140"#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Microsoft Edge";v="140"#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/140.0.0.0"
        )
    ]
);

mod_generator!(
    edge141,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="141", "Not=A?Brand";v="24", "Microsoft Edge";v="141"#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="141", "Not=A?Brand";v="24", "Microsoft Edge";v="141"#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="141", "Not=A?Brand";v="24", "Microsoft Edge";v="141"#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="141", "Not=A?Brand";v="24", "Microsoft Edge";v="141"#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="141", "Not=A?Brand";v="24", "Microsoft Edge";v="141"#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/141.0.0.0"
        )
    ]
);

mod_generator!(
    v135,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/135.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v136,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="136", "Google Chrome";v="136", "Not.A/Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="136", "Google Chrome";v="136", "Not.A/Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="136", "Google Chrome";v="136", "Not.A/Brand";v="99""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="136", "Google Chrome";v="136", "Not.A/Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="136", "Google Chrome";v="136", "Not.A/Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/136.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v137,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Google Chrome";v="137", "Chromium";v="137", "Not/A)Brand";v="24""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="137", "Chromium";v="137", "Not/A)Brand";v="24""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="137", "Chromium";v="137", "Not/A)Brand";v="24""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="137", "Chromium";v="137", "Not/A)Brand";v="24""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="137", "Chromium";v="137", "Not/A)Brand";v="24""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/137.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v138,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Not)A;Brand";v="8", "Chromium";v="138", "Google Chrome";v="138""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not)A;Brand";v="8", "Chromium";v="138", "Google Chrome";v="138""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not)A;Brand";v="8", "Chromium";v="138", "Google Chrome";v="138""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Not)A;Brand";v="8", "Chromium";v="138", "Google Chrome";v="138""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not)A;Brand";v="8", "Chromium";v="138", "Google Chrome";v="138""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/138.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v139,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Not;A=Brand";v="99", "Google Chrome";v="139", "Chromium";v="139""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not;A=Brand";v="99", "Google Chrome";v="139", "Chromium";v="139""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not;A=Brand";v="99", "Google Chrome";v="139", "Chromium";v="139""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Not;A=Brand";v="99", "Google Chrome";v="139", "Chromium";v="139""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not;A=Brand";v="99", "Google Chrome";v="139", "Chromium";v="139""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/139.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v140,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Google Chrome";v="140""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Google Chrome";v="140""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Google Chrome";v="140""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Google Chrome";v="140""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="140", "Not=A?Brand";v="24", "Google Chrome";v="140""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/140.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v141,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Google Chrome";v="141", "Not?A_Brand";v="8", "Chromium";v="141""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="141", "Not?A_Brand";v="8", "Chromium";v="141""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="141", "Not?A_Brand";v="8", "Chromium";v="141""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="141", "Not?A_Brand";v="8", "Chromium";v="141""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="141", "Not?A_Brand";v="8", "Chromium";v="141""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/141.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v142,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="142", "Google Chrome";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="142", "Google Chrome";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="142", "Google Chrome";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="142", "Google Chrome";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="142", "Google Chrome";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/142.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    edge142,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="142", "Microsoft Edge";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="142", "Microsoft Edge";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Mobile Safari/537.36 Edg/142.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="142", "Microsoft Edge";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="142", "Microsoft Edge";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="142", "Microsoft Edge";v="142", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0"
        )
    ]
);

mod_generator!(
    v143,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Google Chrome";v="143", "Chromium";v="143", "Not A(Brand";v="24""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="143", "Chromium";v="143", "Not A(Brand";v="24""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="143", "Chromium";v="143", "Not A(Brand";v="24""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="143", "Chromium";v="143", "Not A(Brand";v="24""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="143", "Chromium";v="143", "Not A(Brand";v="24""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/143.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v144,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Google Chrome";v="144""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Google Chrome";v="144""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Google Chrome";v="144""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Google Chrome";v="144""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Google Chrome";v="144""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/144.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v145,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Not:A-Brand";v="99", "Google Chrome";v="145", "Chromium";v="145""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Not:A-Brand";v="99", "Google Chrome";v="145", "Chromium";v="145""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Not:A-Brand";v="99", "Google Chrome";v="145", "Chromium";v="145""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Not:A-Brand";v="99", "Google Chrome";v="145", "Chromium";v="145""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Not:A-Brand";v="99", "Google Chrome";v="145", "Chromium";v="145""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/145.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v146,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="146", "Not-A.Brand";v="24", "Google Chrome";v="146""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="146", "Not-A.Brand";v="24", "Google Chrome";v="146""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="146", "Not-A.Brand";v="24", "Google Chrome";v="146""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.7680.164 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="146", "Not-A.Brand";v="24", "Google Chrome";v="146""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="146", "Not-A.Brand";v="24", "Google Chrome";v="146""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/146.0.7680.164 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v147,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Google Chrome";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Google Chrome";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Google Chrome";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.7712.122 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Google Chrome";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Google Chrome";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/147.0.7712.122 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    v148,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="148", "Google Chrome";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36"
        ),
        (
            Linux,
            r#""Chromium";v="148", "Google Chrome";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36"
        ),
        (
            Android,
            r#""Chromium";v="148", "Google Chrome";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Mobile Safari/537.36"
        ),
        (
            Windows,
            r#""Chromium";v="148", "Google Chrome";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36"
        ),
        (
            IOS,
            r#""Chromium";v="148", "Google Chrome";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/148.0.0.0 Mobile/15E148 Safari/604.1"
        )
    ]
);

mod_generator!(
    edge143,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="143", "Microsoft Edge";v="143", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="143", "Microsoft Edge";v="143", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Mobile Safari/537.36 Edg/143.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="143", "Microsoft Edge";v="143", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="143", "Microsoft Edge";v="143", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="143", "Microsoft Edge";v="143", "Not_A Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0"
        )
    ]
);

mod_generator!(
    edge144,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Microsoft Edge";v="144""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36 Edg/144.0.0.0"
        ),
        (
            Android,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Microsoft Edge";v="144""#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Mobile Safari/537.36 Edg/144.0.0.0"
        ),
        (
            Windows,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Microsoft Edge";v="144""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36 Edg/144.0.0.0"
        ),
        (
            Linux,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Microsoft Edge";v="144""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36 Edg/144.0.0.0"
        ),
        (
            IOS,
            r#""Not(A:Brand";v="8", "Chromium";v="144", "Microsoft Edge";v="144""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36 Edg/144.0.0.0"
        )
    ]
);

mod_generator!(
    edge145,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="145", "Not;A=Brand";v="24", "Microsoft Edge";v="145""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="145", "Not;A=Brand";v="24", "Microsoft Edge";v="145""#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Mobile Safari/537.36 Edg/145.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="145", "Not;A=Brand";v="24", "Microsoft Edge";v="145""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="145", "Not;A=Brand";v="24", "Microsoft Edge";v="145""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="145", "Not;A=Brand";v="24", "Microsoft Edge";v="145""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0"
        )
    ]
);

mod_generator!(
    edge146,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="146", "Not(A:Brand";v="24", "Microsoft Edge";v="146""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36 Edg/146.0.3856.109"
        ),
        (
            Android,
            r#""Chromium";v="146", "Not(A:Brand";v="24", "Microsoft Edge";v="146""#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.7680.178 Mobile Safari/537.36 EdgA/146.0.3856.97"
        ),
        (
            Windows,
            r#""Chromium";v="146", "Not(A:Brand";v="24", "Microsoft Edge";v="146""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36 Edg/146.0.3856.109"
        ),
        (
            Linux,
            r#""Chromium";v="146", "Not(A:Brand";v="24", "Microsoft Edge";v="146""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36 Edg/146.0.3856.109"
        ),
        (
            IOS,
            r#""Chromium";v="146", "Not(A:Brand";v="24", "Microsoft Edge";v="146""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 EdgiOS/146.0.3856.102 Mobile/15E148 Safari/605.1.15"
        )
    ]
);

mod_generator!(
    edge147,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Microsoft Edge";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36 Edg/147.0.3912.51"
        ),
        (
            Android,
            r#""Microsoft Edge";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.7727.55 Mobile Safari/537.36 EdgA/147.0.3912.51"
        ),
        (
            Windows,
            r#""Microsoft Edge";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36 Edg/147.0.3912.51"
        ),
        (
            Linux,
            r#""Microsoft Edge";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36 Edg/147.0.3912.51"
        ),
        (
            IOS,
            r#""Microsoft Edge";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 EdgiOS/147.0.3912.51 Mobile/15E148 Safari/605.1.15"
        )
    ]
);

mod_generator!(
    edge148,
    v132::build_emulation,
    header_initializer_with_zstd_priority,
    [
        (
            MacOS,
            r#""Chromium";v="148", "Microsoft Edge";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36 Edg/148.0.0.0"
        ),
        (
            Android,
            r#""Chromium";v="148", "Microsoft Edge";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Mobile Safari/537.36 EdgA/148.0.0.0"
        ),
        (
            Windows,
            r#""Chromium";v="148", "Microsoft Edge";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36 Edg/148.0.0.0"
        ),
        (
            Linux,
            r#""Chromium";v="148", "Microsoft Edge";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36 Edg/148.0.0.0"
        ),
        (
            IOS,
            r#""Chromium";v="148", "Microsoft Edge";v="148", "Not/A)Brand";v="99""#,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 EdgiOS/148.0.0.0 Mobile/15E148 Safari/605.1.15"
        )
    ]
);
