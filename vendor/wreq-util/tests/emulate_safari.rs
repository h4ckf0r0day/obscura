#[macro_use]
mod support;

use wreq_util::Emulation;

test_emulation!(
    test_safari_15_3,
    Emulation::Safari15_3,
    ["t13d2614h2_2802a3db6c62_14788d8d241b"],
    "dda308d35f4e5db7b52a61720ca1b122"
);

test_emulation!(
    test_safari_15_5,
    Emulation::Safari15_5,
    ["t13d2614h2_2802a3db6c62_14788d8d241b"],
    "dda308d35f4e5db7b52a61720ca1b122"
);

test_emulation!(
    test_safari_15_6_1,
    Emulation::Safari15_6_1,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "dda308d35f4e5db7b52a61720ca1b122"
);

test_emulation!(
    test_safari_16,
    Emulation::Safari16,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "dda308d35f4e5db7b52a61720ca1b122"
);

test_emulation!(
    test_safari_16_5,
    Emulation::Safari16_5,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "dda308d35f4e5db7b52a61720ca1b122"
);

test_emulation!(
    test_safari_ios_16_5,
    Emulation::SafariIos16_5,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "d5fcbdc393757341115a861bf8d23265"
);

test_emulation!(
    test_safari_17,
    Emulation::Safari17_0,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "959a7e813b79b909a1a0b00a38e8bba3"
);

test_emulation!(
    test_safari_17_2_1,
    Emulation::Safari17_2_1,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "959a7e813b79b909a1a0b00a38e8bba3"
);

test_emulation!(
    test_safari_17_4_1,
    Emulation::Safari17_4_1,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "dda308d35f4e5db7b52a61720ca1b122"
);

test_emulation!(
    test_safari_17_5,
    Emulation::Safari17_5,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "959a7e813b79b909a1a0b00a38e8bba3"
);

test_emulation!(
    test_safari_ios_17_2,
    Emulation::SafariIos17_2,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "ad8424af1cc590e09f7b0c499bf7fcdb"
);

test_emulation!(
    test_safari_ios_17_4_1,
    Emulation::SafariIos17_4_1,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "ad8424af1cc590e09f7b0c499bf7fcdb"
);

test_emulation!(
    test_safari_18,
    Emulation::Safari18,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "d4a2dcbfde511b5040ed5a5190a8d78b"
);

test_emulation!(
    test_safari_18_2,
    Emulation::Safari18_2,
    ["t13d2014h2_a09f3c656075_e42f34c56612"],
    "d4a2dcbfde511b5040ed5a5190a8d78b"
);

test_emulation!(
    test_safari_18_3,
    Emulation::Safari18_3,
    ["t13d2014h2_a09f3c656075_e42f34c56612"],
    "d4a2dcbfde511b5040ed5a5190a8d78b"
);

test_emulation!(
    test_safari_18_3_1,
    Emulation::Safari18_3_1,
    ["t13d2014h2_a09f3c656075_e42f34c56612"],
    "d4a2dcbfde511b5040ed5a5190a8d78b"
);

test_emulation!(
    test_safari_ios_18_1_1,
    Emulation::SafariIos18_1_1,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "d4a2dcbfde511b5040ed5a5190a8d78b"
);

test_emulation!(
    test_safari_ipad_18,
    Emulation::SafariIPad18,
    ["t13d2014h2_a09f3c656075_14788d8d241b"],
    "d4a2dcbfde511b5040ed5a5190a8d78b"
);

test_emulation!(
    test_safari_18_5,
    Emulation::Safari18_5,
    ["t13d2014h2_a09f3c656075_e42f34c56612"],
    "c52879e43202aeb92740be6e8c86ea96"
);

test_emulation!(
    test_safari_26,
    Emulation::Safari26,
    ["t13d2013h2_a09f3c656075_7f0f34a4126d"],
    "c52879e43202aeb92740be6e8c86ea96"
);

test_emulation!(
    test_safari_26_1,
    Emulation::Safari26_1,
    ["t13d2014h2_a09f3c656075_e42f34c56612"],
    "773906b0efdefa24a7f2b8eb6985bf37"
);
