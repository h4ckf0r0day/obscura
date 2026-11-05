#[macro_use]
mod support;

use wreq_util::Emulation;

// Enabling certain extensions will change the length during encryption. This is because TLS will
// automatically use padding to fill the data and add a padding extension. At this time, the ja4
// fingerprint will change.

test_emulation!(
    test_opera116,
    Emulation::Opera116,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera117,
    Emulation::Opera117,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera118,
    Emulation::Opera118,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera119,
    Emulation::Opera119,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera120,
    Emulation::Opera120,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera121,
    Emulation::Opera121,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera122,
    Emulation::Opera122,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera123,
    Emulation::Opera123,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera124,
    Emulation::Opera124,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera125,
    Emulation::Opera125,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera126,
    Emulation::Opera126,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera127,
    Emulation::Opera127,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera128,
    Emulation::Opera128,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera129,
    Emulation::Opera129,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_opera130,
    Emulation::Opera130,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);
