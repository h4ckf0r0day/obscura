// Regression for issue #407: `CdpContext.valid_context_ids` was insert-only,
// so `Runtime.executionContextsCleared` on navigation never pruned the old
// context ids. A `Runtime.callFunctionOn` targeting a pre-navigation context
// still ran (Chrome rejects it with "Cannot find context with specified id"),
// and the set grew by one id per navigation on a long-lived connection.

use obscura_cdp::dispatch::CdpContext;
use obscura_cdp::domains::page::emit_navigation_events;
use obscura_browser::lifecycle::WaitUntil;

fn navigate(ctx: &mut CdpContext, page_id: &str) {
    emit_navigation_events(
        ctx,
        &Some("session-1".to_string()),
        "frame-1",
        "loader-1",
        "http://127.0.0.1/page",
        page_id,
        &[],
        WaitUntil::Load,
        true,
    );
}

#[test]
fn navigation_prunes_stale_execution_context_ids() {
    let mut ctx = CdpContext::new();
    // Seed a stale id that no navigation re-creates (simulates a context from
    // a prior page state, or simply the pre-navigation main id 1).
    ctx.valid_context_ids.insert(999);

    navigate(&mut ctx, "page-1");

    // The stale id is gone after executionContextsCleared; the default world
    // (id 2) and the first isolated world (id 100, counter starts at 100) remain.
    assert!(
        !ctx.valid_context_ids.contains(&999),
        "stale execution context id must be pruned on navigation"
    );
    assert!(ctx.valid_context_ids.contains(&2), "default world id 2 must be re-registered");
    assert!(
        ctx.valid_context_ids.contains(&100),
        "isolated world id 100 must be registered: {:?}",
        ctx.valid_context_ids
    );

    navigate(&mut ctx, "page-1");

    // Second navigation: the first nav's isolated id (100) is now stale, and a
    // fresh id (101) takes its place. The set must not grow across navigations.
    assert!(
        !ctx.valid_context_ids.contains(&100),
        "previous navigation's isolated context id must be pruned"
    );
    assert!(ctx.valid_context_ids.contains(&101), "fresh isolated id 101 must be registered");
    assert!(ctx.valid_context_ids.contains(&2), "default world id 2 must survive navigation");

    // Unbounded-growth check: two navigations leave exactly the default world
    // plus the current isolated world(s), not an accumulating union.
    let count_after_two_navs = ctx.valid_context_ids.len();
    navigate(&mut ctx, "page-1");
    navigate(&mut ctx, "page-1");
    assert_eq!(
        ctx.valid_context_ids.len(),
        count_after_two_navs,
        "valid_context_ids must not grow across navigations (was {count_after_two_navs}, now {})",
        ctx.valid_context_ids.len()
    );
}
