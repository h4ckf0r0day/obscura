#[cfg(not(feature = "render"))]
use std::collections::HashMap;

use obscura_browser::accessibility::build_ax_nodes_with_styles;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    _params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => Ok(json!({})),
        "getFullAXTree" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            #[cfg(feature = "render")]
            let styles = page.js.as_ref().map(|js| js.accessibility_styles()).unwrap_or_default();
            #[cfg(not(feature = "render"))]
            let styles = HashMap::new();
            let nodes = page.with_dom(|dom| build_ax_nodes_with_styles(dom, &styles)).unwrap_or_default();
            Ok(json!({"nodes": nodes}))
        }
        _ => Ok(json!({})),
    }
}
