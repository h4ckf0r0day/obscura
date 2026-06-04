/// Full example: launch, navigate, interact, check cookies.
use obscura::Browser;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let browser = Browser::builder()
        .stealth(true)
        .storage_dir("/tmp/obscura-api-test")
        .build()?;

    let mut page = browser.new_page().await?;
    page.goto("https://example.com").await?;
    println!("URL: {}", page.url());
    println!("Content length: {}", page.content().len());

    let el = page.wait_for_selector("a", Duration::from_secs(5)).await?;
    println!("Link text: {}", el.text());
    println!("Link href: {:?}", el.attribute("href"));

    Ok(())
}
