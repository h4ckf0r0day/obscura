#[macro_use]
mod support;

use wreq_util::Emulation;

test_emulation!(
    test_okhttp3_13,
    Emulation::OkHttp3_13,
    ["t13d1613h2_46e7e9700bed_eca864cca44a"],
    "605a1154008045d7e3cb3c6fb062c0ce"
);

test_emulation!(
    test_okhttp3_14,
    Emulation::OkHttp3_14,
    ["t13d1613h2_46e7e9700bed_eca864cca44a"],
    "605a1154008045d7e3cb3c6fb062c0ce"
);

test_emulation!(
    test_okhttp4_9,
    Emulation::OkHttp4_9,
    ["t13d1513h2_8daaf6152771_eca864cca44a"],
    "605a1154008045d7e3cb3c6fb062c0ce"
);

test_emulation!(
    test_okhttp4_10,
    Emulation::OkHttp4_10,
    ["t13d1613h2_46e7e9700bed_eca864cca44a"],
    "605a1154008045d7e3cb3c6fb062c0ce"
);

test_emulation!(
    test_okhttp4_12,
    Emulation::OkHttp4_12,
    ["t13d1613h2_46e7e9700bed_eca864cca44a"],
    "605a1154008045d7e3cb3c6fb062c0ce"
);

test_emulation!(
    test_okhttp5,
    Emulation::OkHttp5,
    ["t13d1613h2_46e7e9700bed_eca864cca44a"],
    "605a1154008045d7e3cb3c6fb062c0ce"
);
