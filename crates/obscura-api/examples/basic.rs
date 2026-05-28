/// Basic example: launch browser, navigate, extract content.
use obscura_api::Browser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let browser = Browser::new()?;
    let mut page = browser.new_page().await?;

    page.goto("https://example.com").await?;
    println!("URL: {}", page.url());
    println!("Content length: {}", page.content().len());

    Ok(())
}
