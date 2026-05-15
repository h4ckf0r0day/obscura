use obscura_browser::HTML_TO_MARKDOWN_JS;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    _params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "getMarkdown" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let result = page.evaluate(HTML_TO_MARKDOWN_JS);
            let markdown = result.as_str().unwrap_or("").to_string();
            Ok(json!({ "markdown": markdown }))
        }
        _ => Err(format!("Unknown LP method: {}", method)),
    }
}
