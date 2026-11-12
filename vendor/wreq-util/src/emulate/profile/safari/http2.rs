macro_rules! headers_pseudo_order {
    (1) => {
        PseudoOrder::builder()
            .extend([
                PseudoId::Method,
                PseudoId::Scheme,
                PseudoId::Path,
                PseudoId::Authority,
            ])
            .build()
    };
    (2) => {
        PseudoOrder::builder()
            .extend([
                PseudoId::Method,
                PseudoId::Scheme,
                PseudoId::Authority,
                PseudoId::Path,
            ])
            .build()
    };
}

macro_rules! settings_order {
    (1) => {
        SettingsOrder::builder()
            .extend([
                SettingId::HeaderTableSize,
                SettingId::EnablePush,
                SettingId::InitialWindowSize,
                SettingId::MaxConcurrentStreams,
                SettingId::MaxFrameSize,
                SettingId::MaxHeaderListSize,
                SettingId::EnableConnectProtocol,
                SettingId::NoRfc7540Priorities,
            ])
            .build()
    };
    (2) => {
        SettingsOrder::builder()
            .extend([
                SettingId::HeaderTableSize,
                SettingId::EnablePush,
                SettingId::MaxConcurrentStreams,
                SettingId::InitialWindowSize,
                SettingId::MaxFrameSize,
                SettingId::MaxHeaderListSize,
                SettingId::EnableConnectProtocol,
                SettingId::NoRfc7540Priorities,
            ])
            .build()
    };
}

macro_rules! http2_options {
    (@base $builder:expr) => {
        $builder
            .max_concurrent_streams(100)
    };

    (1) => {
        http2_options!(@base Http2Options::builder())
            .initial_window_size(2097152)
            .initial_connection_window_size(10551295)
            .headers_pseudo_order(headers_pseudo_order!(1))
            .settings_order(settings_order!(1))
            .build()
    };
    (2) => {
        http2_options!(@base Http2Options::builder())
            .initial_window_size(2097152)
            .initial_connection_window_size(10551295)
            .enable_push(false)
            .headers_pseudo_order(headers_pseudo_order!(1))
            .settings_order(settings_order!(1))
            .build()
    };
    (3) => {
        http2_options!(@base Http2Options::builder())
            .initial_window_size(2097152)
            .initial_connection_window_size(10485760)
            .enable_push(false)
            .enable_connect_protocol(true)
            .no_rfc7540_priorities(true)
            .headers_pseudo_order(headers_pseudo_order!(2))
            .settings_order(settings_order!(2))
            .build()
    };
    (4) => {
        http2_options!(@base Http2Options::builder())
            .initial_window_size(4194304)
            .initial_connection_window_size(10551295)
            .headers_pseudo_order(headers_pseudo_order!(1))
            .settings_order(settings_order!(1))
            .build()
    };
    (5) => {
        http2_options!(@base Http2Options::builder())
            .initial_window_size(4194304)
            .initial_connection_window_size(10551295)
            .enable_push(false)
            .headers_pseudo_order(headers_pseudo_order!(1))
            .settings_order(settings_order!(1))
            .build()
    };
    (6) => {
        http2_options!(@base Http2Options::builder())
            .initial_window_size(2097152)
            .initial_connection_window_size(10485760)
            .enable_push(false)
            .no_rfc7540_priorities(true)
            .headers_pseudo_order(headers_pseudo_order!(2))
            .settings_order(settings_order!(2))
            .build()
    };
}
