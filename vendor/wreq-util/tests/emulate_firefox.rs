#[macro_use]
mod support;

use wreq_util::Emulation;

// Enabling certain extensions(ECH) will change the length during encryption. This is because TLS will
// automatically use padding to fill the data and add a padding extension. At this time, the ja4
// fingerprint will change.

test_emulation!(
    test_firefox_109,
    Emulation::Firefox109,
    ["t13d1715h2_5b57614c22b0_3d5424432f57"],
    "73d042072ceabaedacfd45e84dff1020"
);

test_emulation!(
    test_firefox_117,
    Emulation::Firefox117,
    ["t13d1715h2_5b57614c22b0_3d5424432f57"],
    "73d042072ceabaedacfd45e84dff1020"
);

test_emulation!(
    test_firefox_128,
    Emulation::Firefox128,
    ["t13d1513h2_8daaf6152771_748f4c70de1c"],
    "1d8a6f51fd7253d04674593073fc18b0"
);

test_emulation!(
    test_firefox_133,
    Emulation::Firefox133,
    ["t13d1716h2_5b57614c22b0_eeeea6562960"],
    "6ea73faa8fc5aac76bded7bd238f6433"
);

test_emulation!(
    test_firefox_135,
    Emulation::Firefox135,
    ["t13d1717h2_5b57614c22b0_3cbfd9057e0d"],
    "6ea73faa8fc5aac76bded7bd238f6433"
);

test_emulation!(
    test_firefox_private_135,
    Emulation::FirefoxPrivate135,
    ["t13d1715h2_5b57614c22b0_a54fffd0eb61"],
    "6ea73faa8fc5aac76bded7bd238f6433"
);

test_emulation!(
    test_firefox_android_135,
    Emulation::FirefoxAndroid135,
    ["t13d1716h2_5b57614c22b0_eeeea6562960"],
    "41a06cadb1c6385e6d08c8d0dbbea818"
);

test_emulation!(
    test_firefox_136,
    Emulation::Firefox136,
    ["t13d1717h2_5b57614c22b0_3cbfd9057e0d"],
    "6ea73faa8fc5aac76bded7bd238f6433"
);

test_emulation!(
    test_firefox_private_136,
    Emulation::FirefoxPrivate136,
    ["t13d1715h2_5b57614c22b0_a54fffd0eb61"],
    "6ea73faa8fc5aac76bded7bd238f6433"
);

test_emulation!(
    test_firefox_139,
    Emulation::Firefox139,
    ["t13d1717h2_5b57614c22b0_3cbfd9057e0d"],
    "6ea73faa8fc5aac76bded7bd238f6433"
);

test_emulation!(
    test_firefox_142,
    Emulation::Firefox142,
    ["t13d1717h2_5b57614c22b0_3cbfd9057e0d"],
    "6ea73faa8fc5aac76bded7bd238f6433"
);

test_emulation!(
    test_firefox_143,
    Emulation::Firefox143,
    ["t13d1717h2_5b57614c22b0_3cbfd9057e0d"],
    "6ea73faa8fc5aac76bded7bd238f6433"
);
