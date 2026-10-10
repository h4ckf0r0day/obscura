// One link of the browser engine. Nextest still starts a process per test.
macro_rules! integration_tests {
    ($($module:ident),* $(,)?) => {
        $(mod $module;)*

        #[test]
        fn every_integration_file_is_registered() {
            use std::collections::BTreeSet;
            let registered: BTreeSet<String> = [$(stringify!($module).to_owned(),)*]
                .into_iter().collect();
            let files: BTreeSet<String> = std::fs::read_dir(
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests"),
            ).unwrap().map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
                .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
                .filter(|name| name != "integration")
                .collect();
            assert_eq!(registered, files, "register new test files in integration_tests!");
        }
    };
}

integration_tests!(
    accept_thread_survives_silent_connections,
    accessibility_names,
    backspace_surrogate,
    binding_called_session,
    cdp_click_submit_parity,
    child_frame_tree,
    concurrent_connections_heavy_page,
    concurrent_navigations,
    concurrent_navigations_with_fetch,
    concurrent_page_isolation,
    control_plane_unblocked,
    document_write_lifecycle,
    dynamic_script_onload_fires,
    dynamic_stylesheet_onload_fires,
    execution_context_ownership,
    execution_context_pruned_on_navigation,
    file_navigation_gate,
    form_submit_method_bypasses_listener,
    iframe_event_dispatch,
    input_activation_author_accessors,
    input_key_event_escaping,
    input_mouse_event_parity,
    input_mouse_label_activation,
    js_fetch_emits_network_events,
    max_connections_cap,
    nodefilter_constants,
    page_frame_contract,
    runtime_by_value_undefined,
    runtime_console_events,
    runtime_get_properties_objectid_escaping,
    runtime_primitives,
    scroll_event_on_assignment,
    structured_clone_crypto_parity,
    textarea_enter_selection,
    treewalker_document_order,
    window_conformance_parity,
);
