use std::{
    cell::Cell,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
};

// from: https://github.com/seanmonstar/reqwest/blob/5d5bf355744b181d31533501133ad9fbf99e8849/src/util.rs#L28
pub(crate) fn fast_random() -> u64 {
    thread_local! {
        static KEY: RandomState = RandomState::new();
        static COUNTER: Cell<u64> = const { Cell::new(0) };
    }

    KEY.with(|key| {
        COUNTER.with(|ctr| {
            let n = ctr.get().wrapping_add(1);
            ctr.set(n);

            let mut h = key.build_hasher();
            h.write_u64(n);
            h.finish()
        })
    })
}
