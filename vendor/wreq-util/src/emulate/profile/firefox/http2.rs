macro_rules! headers_stream_dependency {
    (1) => {
        StreamDependency::new(StreamId::zero(), 21, false)
    };
    (2) => {
        StreamDependency::new(StreamId::from(13), 21, false)
    };
}

macro_rules! pseudo_order {
    () => {
        PseudoOrder::builder()
            .extend([
                PseudoId::Method,
                PseudoId::Path,
                PseudoId::Authority,
                PseudoId::Scheme,
            ])
            .build()
    };
}

macro_rules! settings_order {
    () => {
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
            .initial_window_size(131072)
            .max_frame_size(16384)
            .initial_connection_window_size(12517377 + 65535)
            .headers_pseudo_order(pseudo_order!())
            .settings_order(settings_order!())
    };

    (1) => {
        http2_options!(@base Http2Options::builder())
            .initial_stream_id(3)
            .header_table_size(65536)
            .enable_push(false)
            .headers_stream_dependency(headers_stream_dependency!(1))
            .build()
    };
    (2) => {
        http2_options!(@base Http2Options::builder())
            .initial_stream_id(15)
            .header_table_size(65536)
            .headers_stream_dependency(headers_stream_dependency!(2))
            .priorities(
                Priorities::builder()
                    .extend([
                        Priority::new(
                            StreamId::from(3),
                            StreamDependency::new(StreamId::zero(), 200, false),
                        ),
                        Priority::new(
                            StreamId::from(5),
                            StreamDependency::new(StreamId::zero(), 100, false),
                        ),
                        Priority::new(
                            StreamId::from(7),
                            StreamDependency::new(StreamId::zero(), 0, false),
                        ),
                        Priority::new(
                            StreamId::from(9),
                            StreamDependency::new(StreamId::from(7), 0, false),
                        ),
                        Priority::new(
                            StreamId::from(11),
                            StreamDependency::new(StreamId::from(3), 0, false),
                        ),
                        Priority::new(
                            StreamId::from(13),
                            StreamDependency::new(StreamId::zero(), 240, false),
                        ),
                    ])
                    .build(),
            )
            .build()
    };
    (3) => {
        http2_options!(@base Http2Options::builder())
            .initial_stream_id(3)
            .header_table_size(65536)
            .enable_push(false)
            .max_concurrent_streams(0)
            .headers_stream_dependency(headers_stream_dependency!(1))
            .build()
    };
    (4) => {
        http2_options!(@base Http2Options::builder())
            .initial_stream_id(3)
            .header_table_size(4096)
            .enable_push(false)
            .initial_window_size(32768)
            .headers_stream_dependency(headers_stream_dependency!(1))
            .build()
    };
}
