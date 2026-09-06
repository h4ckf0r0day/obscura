use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};

#[tokio::test(flavor = "current_thread")]
async fn input_accept_reflects_the_content_attribute() {
    let context = Arc::new(BrowserContext::with_storage_and_network(
        "input-accept".to_owned(),
        None,
        false,
        None,
        None,
        true,
    ));
    let mut page = Page::new("input-accept-page".to_owned(), context);
    page.navigate("data:text/html,<input id=upload type=file accept='image/png,image/jpeg'>")
        .await
        .unwrap();

    assert_eq!(
        page.evaluate(
            r#"
            (() => {
                const input = document.getElementById('upload');
                const initial = input.accept;
                input.accept = 'image/webp';
                return {
                    initial,
                    property: input.accept,
                    attribute: input.getAttribute('accept'),
                };
            })()
            "#,
        ),
        serde_json::json!({
            "initial": "image/png,image/jpeg",
            "property": "image/webp",
            "attribute": "image/webp",
        })
    );
}
