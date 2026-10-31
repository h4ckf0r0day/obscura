#[macro_use]
mod support;

use wreq_util::Emulation;

// Enabling certain extensions(ECH) will change the length during encryption. This is because TLS will
// automatically use padding to fill the data and add a padding extension. At this time, the ja4
// fingerprint will change.

test_emulation!(
    test_chrome100,
    Emulation::Chrome100,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "4f04edce68a7ecbe689edce7bf5f23f3"
);

test_emulation!(
    test_chrome101,
    Emulation::Chrome101,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "4f04edce68a7ecbe689edce7bf5f23f3"
);

test_emulation!(
    test_edge101,
    Emulation::Edge101,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "4f04edce68a7ecbe689edce7bf5f23f3"
);

test_emulation!(
    test_chrome104,
    Emulation::Chrome104,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "4f04edce68a7ecbe689edce7bf5f23f3"
);

test_emulation!(
    test_chrome105,
    Emulation::Chrome105,
    [
        "t13d1516h2_8daaf6152771_02713d6af862",
        "t13d1517h2_8daaf6152771_b1ff8ab2d16f"
    ],
    "4f04edce68a7ecbe689edce7bf5f23f3"
);

test_emulation!(
    test_chrome106,
    Emulation::Chrome106,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "a345a694846ad9f6c97bcc3c75adbe26"
);

test_emulation!(
    test_chrome107,
    Emulation::Chrome107,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "a345a694846ad9f6c97bcc3c75adbe26"
);

test_emulation!(
    test_chrome108,
    Emulation::Chrome108,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "a345a694846ad9f6c97bcc3c75adbe26"
);

test_emulation!(
    test_chrome109,
    Emulation::Chrome109,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "a345a694846ad9f6c97bcc3c75adbe26"
);

test_emulation!(
    test_chrome110,
    Emulation::Chrome110,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "4f04edce68a7ecbe689edce7bf5f23f3"
);

test_emulation!(
    test_chrome114,
    Emulation::Chrome114,
    ["t13d1516h2_8daaf6152771_e5627efa2ab1"],
    "a345a694846ad9f6c97bcc3c75adbe26"
);

test_emulation!(
    test_chrome116,
    Emulation::Chrome116,
    [
        "t13d1516h2_8daaf6152771_02713d6af862",
        "t13d1517h2_8daaf6152771_b1ff8ab2d16f"
    ],
    "a345a694846ad9f6c97bcc3c75adbe26"
);

test_emulation!(
    test_chrome117,
    Emulation::Chrome117,
    [
        "t13d1516h2_8daaf6152771_02713d6af862",
        "t13d1517h2_8daaf6152771_b1ff8ab2d16f"
    ],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome118,
    Emulation::Chrome118,
    [
        "t13d1516h2_8daaf6152771_02713d6af862",
        "t13d1517h2_8daaf6152771_b1ff8ab2d16f"
    ],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome119,
    Emulation::Chrome119,
    [
        "t13d1516h2_8daaf6152771_02713d6af862",
        "t13d1517h2_8daaf6152771_b1ff8ab2d16f"
    ],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome120,
    Emulation::Chrome120,
    [
        "t13d1516h2_8daaf6152771_02713d6af862",
        "t13d1517h2_8daaf6152771_b1ff8ab2d16f"
    ],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_edge122,
    Emulation::Edge122,
    [
        "t13d1516h2_8daaf6152771_02713d6af862",
        "t13d1517h2_8daaf6152771_b1ff8ab2d16f"
    ],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome123,
    Emulation::Chrome123,
    [
        "t13d1516h2_8daaf6152771_02713d6af862",
        "t13d1517h2_8daaf6152771_b1ff8ab2d16f"
    ],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome124,
    Emulation::Chrome124,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome126,
    Emulation::Chrome126,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome127,
    Emulation::Chrome127,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_edge127,
    Emulation::Edge127,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome128,
    Emulation::Chrome128,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome129,
    Emulation::Chrome129,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome130,
    Emulation::Chrome130,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome131,
    Emulation::Chrome131,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_edge131,
    Emulation::Edge131,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome132,
    Emulation::Chrome132,
    ["t13d1516h2_8daaf6152771_d8a2da3f94cd"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome133,
    Emulation::Chrome133,
    ["t13d1516h2_8daaf6152771_d8a2da3f94cd"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome134,
    Emulation::Chrome134,
    ["t13d1516h2_8daaf6152771_d8a2da3f94cd"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_edge134,
    Emulation::Edge134,
    ["t13d1516h2_8daaf6152771_02713d6af862"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome135,
    Emulation::Chrome135,
    ["t13d1516h2_8daaf6152771_d8a2da3f94cd"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome136,
    Emulation::Chrome136,
    ["t13d1516h2_8daaf6152771_d8a2da3f94cd"],
    "52d84b11737d980aef856699f885ca86"
);

test_emulation!(
    test_chrome137,
    Emulation::Chrome137,
    ["t13d1516h2_8daaf6152771_d8a2da3f94cd"],
    "52d84b11737d980aef856699f885ca86"
);
