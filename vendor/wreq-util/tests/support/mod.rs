#![allow(dead_code)]
pub mod server;

use std::{sync::LazyLock, time::Duration};

use tokio::sync::Semaphore;
use wreq::Client;

// TODO: remove once done converting to new support server?
#[allow(unused)]
pub static DEFAULT_USER_AGENT: &str =
    concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

pub static TEST_SEMAPHORE: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(1));

pub static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .connect_timeout(Duration::from_secs(60))
        .build()
        .unwrap()
});

#[allow(unused_macros)]
macro_rules! test_emulation {
    ($test_name:ident, $emulation:expr, $ja4:expr, $akamai_hash:expr) => {
        #[tokio::test]
        async fn $test_name() {
            let _permit = crate::support::TEST_SEMAPHORE.acquire().await.unwrap();

            let resp = crate::support::CLIENT
                .get("https://tls.browserleaks.com/")
                .emulation($emulation)
                .send()
                .await
                .unwrap();

            assert_eq!(resp.status(), wreq::StatusCode::OK);
            let content = resp.text().await.unwrap();

            let conditional = $ja4.iter().any(|&ja4| content.contains(ja4));
            if !conditional {
                println!("{}", content);
            }
            assert!(conditional);

            let conditional = content.contains($akamai_hash);
            if !conditional {
                println!("{}", content);
            }
            assert!(conditional);

            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }
    };
}
