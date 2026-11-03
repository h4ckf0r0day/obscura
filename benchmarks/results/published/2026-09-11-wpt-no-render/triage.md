WPT triage: 38532 files, 31485 failing files, 3492 unsupported files, 289331 failing subtests.

## Conformance tiers

| tier | files ok | subtests pass | role |
| ---- | -------- | ------------- | ---- |
| Core | 1124/7728 (14.5%) | 327094/392547 (83.3%) | headline + SLA |
| Relevant | 2053/15443 (13.3%) | 521393/600709 (86.8%) | Core + broader correctness |
| Full | 3555/35040 (10.1%) | 603997/893328 (67.6%) | whole suite (transparency) |
| (excluded) | 1388/15944 (8.7%) | 44584/250853 (17.8%) | out of scope, no target |
| (unclassified) | 114/3653 (3.1%) | 38020/41766 (91.0%) | not yet tiered, extend manifest |

## Top error signatures

| # | count | suspected subsystem | areas | signature |
| - | ----- | ------------------- | ----- | --------- |
| 1 | 113883 | unclassified | FileAPI, IndexedDB, WebCryptoAPI, animation-worklet, apng, audio-session, compat, connection-allowlist, console, content-dpr, content-security-policy, cookies, cors, css, custom-elements, document-policy, dom, domparsing, domxpath, editing, encoding, encoding-detection, event-timing, eventsource, fetch, focus, forced-colors-mode, fullscreen, gamepad, hr-time, html, html-media-capture, imagebitmap-renderingcontext, inert, infrastructure, input-device-capabilities, input-events, intersection-observer, is-input-pending, jpegxl, largest-contentful-paint, layout-instability, mathml, media-playback-quality, media-source, mediacapture-fromelement, mediacapture-record, mediasession, mimesniff, mst-content-hint, navigation-api, navigation-timing, netinfo, notifications, old-tests, page-visibility, paint-timing, performance-timeline, permissions, permissions-policy, png, pointerevents, pointerlock, preload, quirks, referrer-policy, remote-playback, requestidlecallback, resize-observer, resource-timing, sanitizer-api, scheduler, scroll-animations, scroll-to-text-fragment, selection, shadow-dom, streams, subresource-integrity, svg, touch-events, trusted-types, uievents, url, urlpattern, user-timing, virtual-keyboard, visual-viewport, wai-aria, wasm, web-animations, webaudio, webcodecs, webidl, webmessaging, webrtc, webrtc-extensions, webrtc-priority, websockets, webstorage, webvtt, workers, x-frame-options, xhr, xml | assert_equals: expected <v> but got <v> |
| 2 | 70929 | unclassified | FileAPI, IndexedDB, connection-allowlist, content-security-policy, contenteditable, cors, css, custom-elements, dom, domparsing, ecmascript, editing, fetch, fullscreen, html, infrastructure, intersection-observer, media-capabilities, mediacapture-streams, selection, shadow-dom, svg, uievents, web-locks, web-share, webaudio, webauthn, webidl, webrtc, websockets, xhr | assert_false: expected <v> |
| 3 | 16433 | unclassified | FileAPI, IndexedDB, acid, compat, connection-allowlist, console, content-security-policy, contenteditable, cors, css, custom-elements, dom, domparsing, domxpath, editing, encoding, entries-api, eventsource, fetch, fullscreen, gamepad, hr-time, html, html-media-capture, import-maps, infrastructure, input-device-capabilities, input-events, intersection-observer, jpegxl, loading, longtask-timing, mathml, media-source, mediacapture-fromelement, mimesniff, mst-content-hint, navigation-api, navigation-timing, page-lifecycle, page-visibility, performance-timeline, permissions-policy, pointerevents, pointerlock, preload, private-click-measurement, remote-playback, requestidlecallback, resize-observer, sanitizer-api, screen-orientation, scroll-animations, scroll-to-text-fragment, selection, shadow-dom, speculation-rules, streams, svg, touch-events, uievents, url, wai-aria, wasm, web-animations, web-share, webaudio, webgl, webidl, webmessaging, webrtc, websockets, webvtt, workers, xhr, xml | assert_true: expected <v> |
| 4 | 15929 | unclassified | compat, css, html, quirks | assert_equals: |
| 5 | 7078 | obscura-js event loop | FileAPI, IndexedDB, clipboard-apis, close-watcher, compression, connection-allowlist, content-security-policy, cookies, cors, css, custom-elements, density-size-correction, document-policy, dom, domparsing, domxpath, encrypted-media, fetch, focus, fullscreen, hr-time, html, html-aam, imagebitmap-renderingcontext, import-maps, infrastructure, js, js-self-profiling, largest-contentful-paint, layout-instability, mathml, media-playback-quality, media-source, mediacapture-fromelement, mediacapture-image, mediacapture-record, mediasession, mimesniff, mst-content-hint, navigation-api, navigation-timing, permissions, permissions-policy, referrer-policy, resize-observer, resource-timing, sanitizer-api, scheduler, scroll-animations, shadow-dom, signed-exchange, speculation-rules, storage-access-api, streams, svg, trusted-types, video-rvfc, wasm, web-animations, webaudio, webcodecs, webidl, webmessaging, webrtc, webrtc-encoded-transform, webrtc-extensions, webrtc-priority, webrtc-stats, webrtc-svc, websockets, xhr | promise_test: Unhandled rejection with value: object <v> |
| 6 | 6685 | unclassified | FileAPI, IndexedDB, WebCryptoAPI, autoplay-policy-detection, beacon, clear-site-data, client-hints, clipboard-apis, compression, connection-allowlist, console, content-security-policy, cookies, cors, css, custom-elements, delegated-ink, density-size-correction, deprecation-reporting, digital-credentials, document-policy, dom, domparsing, domxpath, editing, encoding, encoding-detection, event-timing, eventsource, fetch, focus, fullscreen, gamepad, gpc, hr-time, hsts, html, https-upgrades, import-maps, inert, infrastructure, intersection-observer, intervention-reporting, jpegxl, js, largest-contentful-paint, layout-instability, longtask-timing, mathml, media-capabilities, media-source, mediacapture-fromelement, mediacapture-image, mediacapture-insertable-streams, mediacapture-record, mimesniff, mixed-content, navigation-api, navigation-timing, notifications, old-tests, page-lifecycle, page-visibility, paint-timing, performance-timeline, permissions, permissions-policy, png, preload, referrer-policy, reporting, requestidlecallback, resize-observer, resource-timing, sanitizer-api, savedata, scheduler, screen-wake-lock, scroll-animations, scroll-to-text-fragment, secure-contexts, selection, service-workers, shadow-dom, signed-exchange, speculation-rules, speech-api, storage-access-api, streams, svg, timing-entrytypes-registry, trusted-types, uievents, url, urlpattern, user-timing, visual-viewport, wasm, web-animations, web-extensions, web-locks, webaudio, webcodecs, webidl, webmessaging, webrtc, webrtc-stats, websockets, webstorage, webusb, webvtt, webxr, workers, x-frame-options, xhr, xml | <harness error> |
| 7 | 5887 | unclassified | FileAPI, IndexedDB, animation-worklet, audio-session, autoplay-policy-detection, beacon, compat, console, content-security-policy, css, dom, entries-api, event-timing, fetch, fullscreen, gamepad, gpc, hr-time, html, input-device-capabilities, input-events, is-input-pending, largest-contentful-paint, layout-instability, longtask-timing, mathml, measure-memory, media-capabilities, media-playback-quality, media-source, mediacapture-fromelement, mediacapture-record, mediasession, navigation-timing, netinfo, paint-timing, permissions, permissions-policy, permissions-request, permissions-revoke, pointerlock, referrer-policy, remote-playback, reporting, requestidlecallback, resource-timing, screen-orientation, scroll-animations, scroll-to-text-fragment, selection, storage-access-api, svg, touch-events, trusted-types, uievents, user-timing, vibration, video-rvfc, wai-aria, wasm, web-animations, webdriver, webgl, webidl, websockets, webvtt, xhr | assert_own_property: expected <v> |
| 8 | 5776 | unclassified | FileAPI, IndexedDB, WebCryptoAPI, accelerometer, ai, ambient-light, animation-worklet, audio-output, background-fetch, background-sync, badging, battery-status, beacon, bluetooth, captured-mouse-events, client-hints, clipboard-apis, compression, compute-pressure, connection-allowlist, contacts, content-index, content-security-policy, cookies, cookiestore, cpu-performance, credential-management, css, custom-elements, device-memory, device-posture, direct-sockets, dom, domxpath, encoding, encrypted-media, eventsource, eyedropper, fetch, file-system-access, focus, font-access, fs, gamepad, generic-sensor, geolocation, gyroscope, hr-time, html, https-upgrades, idle-detection, import-maps, infrastructure, installedapp, intersection-observer, keyboard-lock, keyboard-map, long-animation-frame, magnetometer, managed, measure-memory, media-capabilities, media-source, mediacapture-handle, mediacapture-image, mediacapture-streams, mimesniff, mixed-content, navigation-api, navigation-timing, netinfo, notifications, orientation-event, orientation-sensor, page-visibility, payment-method-basic-card, payment-method-id, payment-method-manifest, payment-request, performance-timeline, periodic-background-sync, permissions, permissions-policy, permissions-request, permissions-revoke, picture-in-picture, pointerevents, preload, proximity, push-api, referrer-policy, reporting, requestidlecallback, resize-observer, resource-timing, screen-capture, screen-details, screen-wake-lock, scroll-to-text-fragment, serial, server-timing, service-workers, shadow-dom, shape-detection, speculation-rules, speech-api, storage, storage-access-api, streams, subapps, subresource-integrity, svg, trusted-types, ua-client-hints, upgrade-insecure-requests, url, urlpattern, user-timing, virtual-keyboard, wasm, web-based-payment-handler, web-bundle, web-locks, web-nfc, web-otp, web-share, webaudio, webauthn, webcodecs, webgl, webhid, webmcp, webmessaging, webmidi, webnn, webrtc, webrtc-encoded-transform, webrtc-identity, webrtc-stats, websockets, webstorage, webtransport, webusb, webxr, workers, worklets, x-frame-options, xhr, xml | no result before timeout |
| 9 | 3675 | unclassified | FileAPI, IndexedDB, autoplay-policy-detection, beacon, clear-site-data, connection-allowlist, content-security-policy, cookies, css, custom-elements, density-size-correction, document-policy, dom, domparsing, encoding, eventsource, fetch, focus, fullscreen, html, https-upgrades, import-maps, inert, infrastructure, intersection-observer, largest-contentful-paint, mathml, mediacapture-record, mimesniff, mixed-content, navigation-api, navigation-timing, page-visibility, performance-timeline, permissions-policy, png, preload, referrer-policy, reporting, resize-observer, resource-timing, sanitizer-api, scroll-animations, secure-contexts, selection, shadow-dom, speculation-rules, streams, svg, timing-entrytypes-registry, trusted-types, uievents, url, urlpattern, user-timing, wasm, web-animations, webaudio, webcodecs, webmessaging, webrtc, websockets, workers, xhr | <empty message> |
| 10 | 3471 | unclassified | encoding | Failed to execute <v> on <v>: The encoded data was not valid. |
| 11 | 2926 | missing/incomplete JS API (bootstrap.js) | IndexedDB, accelerometer, ambient-light, compute-pressure, content-security-policy, css, custom-elements, delegated-ink, dom, domparsing, encoding, encrypted-media, event-timing, fetch, font-access, gamepad, gyroscope, html, largest-contentful-paint, layout-instability, loading, magnetometer, mediacapture-streams, mediasession, navigation-api, navigation-timing, paint-timing, performance-timeline, permissions-policy, remote-playback, resource-timing, sanitizer-api, screen-wake-lock, scroll-animations, shadow-dom, streams, svg, trusted-types, user-timing, visual-viewport, wasm, web-animations, webaudio, webidl, webmessaging, webnn, webusb, workers, xhr | Cannot read properties of undefined (reading <v>) |
| 12 | 2779 | unclassified | FileAPI, IndexedDB, autoplay-policy-detection, beacon, clear-site-data, client-hints, connection-allowlist, content-security-policy, cookies, cors, css, custom-elements, density-size-correction, dom, domparsing, encoding, eventsource, fetch, focus, fullscreen, gamepad, hsts, html, import-maps, infrastructure, intersection-observer, jpegxl, js, largest-contentful-paint, layout-instability, longtask-timing, mathml, mediacapture-image, mimesniff, navigation-api, navigation-timing, notifications, page-lifecycle, page-visibility, paint-timing, performance-timeline, permissions-policy, preload, referrer-policy, reporting, requestidlecallback, resize-observer, resource-timing, sanitizer-api, scheduler, scroll-animations, scroll-to-text-fragment, secure-contexts, selection, service-workers, shadow-dom, signed-exchange, speculation-rules, speech-api, storage-access-api, streams, svg, trusted-types, uievents, url, urlpattern, user-timing, visual-viewport, wasm, web-animations, webaudio, webcodecs, webmessaging, webrtc, webrtc-stats, websockets, webstorage, webvtt, webxr, workers, x-frame-options, xhr | Test timed out |
| 13 | 2554 | unclassified | css, fetch, html, intersection-observer, mathml, scroll-animations, svg, web-animations, webaudio | assert_approx_equals: expected <v> but got <v> |
| 14 | 1980 | missing/incomplete JS API (bootstrap.js) | IndexedDB, content-security-policy, cors, css, custom-elements, dom, domparsing, editing, fetch, html, imagebitmap-renderingcontext, intersection-observer, selection, shadow-dom, trusted-types, webgl, webstorage, xhr | Cannot read properties of null (reading <v>) |
| 15 | 1899 | obscura-dom (tree.rs / bootstrap.js) | dom, selection | iframe.contentDocument.appendChild is not a function |
| 16 | 1725 | missing/incomplete JS API (bootstrap.js) | html | root.normalize is not a function |
| 17 | 1685 | unclassified | IndexedDB, css, custom-elements, dom, encoding, fetch, html, import-maps, mathml, streams, uievents, url, web-animations, webaudio, webmessaging, webstorage, xhr | assert_array_equals: expected <v> but got <v> |
| 18 | 1344 | unclassified | IndexedDB, css, custom-elements, dom, domparsing, domxpath, html, input-events, intersection-observer, performance-timeline, shadow-dom, streams, svg, user-timing, wasm, web-animations, webaudio, webcodecs, webmessaging, webrtc, webrtc-extensions, webrtc-svc, websockets, webvtt, workers, xhr | assert_throws_dom: function <v> |
| 19 | 1255 | unclassified | css, html | Colors do not match. |
| 20 | 1121 | unclassified | FileAPI, audio-session, compat, content-security-policy, css, custom-elements, dom, entries-api, event-timing, fullscreen, gpc, hr-time, html, html-media-capture, input-events, intersection-observer, media-playback-quality, mediacapture-fromelement, mediasession, navigation-timing, page-lifecycle, permissions, permissions-policy, permissions-request, permissions-revoke, pointerlock, private-click-measurement, resource-timing, screen-orientation, scroll-to-text-fragment, selection, storage-access-api, svg, touch-events, uievents, vibration, video-rvfc, wai-aria, wasm, web-animations, websockets, xhr | assert_inherits: property <v> not found in prototype chain |
| 21 | 1015 | unclassified | content-security-policy, css, dom, fetch, html, infrastructure, url, web-animations, xhr | assert_unreached: expected <v> |
| 22 | 1009 | unclassified | FileAPI, IndexedDB, css, custom-elements, dom, domparsing, domxpath, encoding, fetch, html, import-maps, navigation-api, preload, shadow-dom, streams, uievents, url, wai-aria, wasm, web-animations, webidl, webrtc, xhr | assert_array_equals: expected <v> |
| 23 | 974 | unclassified | css | assert_not_equals: initial and target values may not match got disallowed value <v> |
| 24 | 956 | obscura-dom (tree.rs / bootstrap.js) | css, html, webvtt | assert_throws_dom: <v> should throw in querySelector function <v> did not throw |
| 25 | 809 | obscura-net / bootstrap.js fetch shim | css, html, navigation-api, resource-timing, url | Failed to construct <v>: Invalid URL |
| 26 | 715 | unclassified | css | assert_in_array: gridTemplateColumns value <v> not in array [<v>] |
| 27 | 657 | unclassified | IndexedDB, content-security-policy, cors, css, custom-elements, dom, domxpath, fullscreen, html, url, user-timing, wasm, webmessaging, websockets, workers, xhr | assert_throws_dom: function <v> did not throw |
| 28 | 640 | unclassified | FileAPI, IndexedDB, content-security-policy, css, dom, encoding, hr-time, html, intersection-observer, mathml, media-capabilities, navigation-timing, netinfo, permissions, requestidlecallback, resize-observer, screen-orientation, selection, svg, uievents, url, web-animations, webidl, webmessaging, websockets, webvtt, workers, xhr | assert_class_string: expected <v> but got <v> |
| 29 | 632 | unclassified | css | assert_true: position-area doesn<v> |
| 30 | 588 | obscura-dom (tree.rs / bootstrap.js) | dom | element[method] is not a function |

### samples

1. `assert_equals: expected <v> but got <v>`
   - FileAPI/blob/Blob-constructor-detached-buffer.any.js::Blob from a detached ArrayBufferView should be empty
   - FileAPI/blob/Blob-constructor-detached-buffer.any.js::Blob from a detached ArrayBufferView with offset should be empty
   - FileAPI/blob/Blob-constructor-detached-buffer.any.js::Blob from a detached ArrayBuffer should be empty
2. `assert_false: expected <v>`
   - FileAPI/FileReader/progress_event_bubbles_cancelable.html::Check the values of bubbles and cancelable are false when the progress event is dispatched
   - FileAPI/idlharness.any.js::FileReader interface: constant EMPTY on interface object
   - FileAPI/idlharness.any.js::FileReader interface: constant EMPTY on interface prototype object
3. `assert_true: expected <v>`
   - FileAPI/blob/Blob-textStream.any.js::textStream method existence
   - FileAPI/file/send-file-formdata-controls.any.js::Upload file-for-upload-in-form-LF-[
].txt (ASCII) in fetch with FormData
   - FileAPI/file/send-file-formdata-controls.any.js::Upload file-for-upload-in-form-LF-CR-[
].txt (ASCII) in fetch with FormData
4. `assert_equals:`
   - compat/webkit-box-item-shrink-001.html::.webkit-box 1
   - compat/webkit-box-item-shrink-001.html::.webkit-box 2
   - compat/webkit-box-item-shrink-001.html::.webkit-box 3
5. `promise_test: Unhandled rejection with value: object <v>`
   - FileAPI/blob/Blob-constructor-dom.window.js::Passing an platform object that supports indexed properties as the blobParts array should work (select).
   - FileAPI/blob/Blob-stream.any.js::Blob.stream()
   - FileAPI/blob/Blob-stream.any.js::Blob.stream() empty Blob
6. `<harness error>`
   - FileAPI/Blob-methods-from-detached-frame.html::<harness>
   - FileAPI/BlobURL/opaque-origin.html::<harness>
   - FileAPI/FileReader/workers.html::<harness>
7. `assert_own_property: expected <v>`
   - FileAPI/idlharness.any.js::Blob interface: operation stream()
   - FileAPI/idlharness.any.js::Blob interface: operation textStream()
   - FileAPI/idlharness.any.js::FileList interface: existence and properties of interface object
8. `no result before timeout`
   - FileAPI/idlharness.any.js::<harness>
   - FileAPI/url/sandboxed-iframe.html::<harness>
   - FileAPI/url/url-format.any.js::<harness>
9. `<empty message>`
   - FileAPI/Blob-methods-from-detached-frame.html::text()
   - FileAPI/Blob-methods-from-detached-frame.html::arrayBuffer()
   - FileAPI/Blob-methods-from-detached-frame.html::bytes()
10. `Failed to execute <v> on <v>: The encoded data was not valid.`
   - encoding/textdecoder-fatal-single-byte.any.js::Not throw: IBM866 has a pointer 128
   - encoding/textdecoder-fatal-single-byte.any.js::Not throw: IBM866 has a pointer 129
   - encoding/textdecoder-fatal-single-byte.any.js::Not throw: IBM866 has a pointer 130

## By spec area

| area | failing subtests | files affected |
| ---- | ---------------- | -------------- |
| editing | 93202 | 86 |
| css | 90185 | 6745 |
| html | 41153 | 6351 |
| dom | 12813 | 469 |
| shadow-dom | 7150 | 130 |
| encoding | 5593 | 130 |
| fetch | 3853 | 444 |
| svg | 3595 | 722 |
| mathml | 2977 | 187 |
| url | 2859 | 33 |
| referrer-policy | 2441 | 1174 |
| webgl | 2103 | 9 |
| IndexedDB | 1949 | 226 |
| websockets | 1884 | 190 |
| custom-elements | 1792 | 178 |
| scroll-animations | 1687 | 219 |
| content-security-policy | 1571 | 741 |
| selection | 1521 | 69 |
| domparsing | 1483 | 69 |
| web-animations | 1439 | 128 |
| mimesniff | 1256 | 6 |
| streams | 1215 | 86 |
| webaudio | 1214 | 291 |
| xhr | 1138 | 283 |
| webrtc | 984 | 208 |
| workers | 897 | 247 |
| pointerevents | 851 | 16 |
| wasm | 678 | 152 |
| quirks | 671 | 13 |
| trusted-types | 649 | 216 |
| navigation-api | 623 | 432 |
| webnn | 576 | 180 |
| resource-timing | 573 | 144 |
| speculation-rules | 522 | 176 |
| media-source | 491 | 81 |
| webcodecs | 480 | 70 |
| sanitizer-api | 454 | 30 |
| urlpattern | 434 | 9 |
| FileAPI | 414 | 69 |
| mixed-content | 390 | 388 |
| compression | 378 | 20 |
| WebCryptoAPI | 332 | 130 |
| service-workers | 328 | 290 |
| mediacapture-record | 320 | 12 |
| intersection-observer | 312 | 133 |
| cors | 293 | 19 |
| navigation-timing | 288 | 52 |
| webvtt | 280 | 73 |
| webmessaging | 277 | 132 |
| preload | 230 | 64 |
| user-timing | 223 | 38 |
| webidl | 223 | 40 |
| xml | 214 | 8 |
| subresource-integrity | 206 | 23 |
| upgrade-insecure-requests | 197 | 197 |
| connection-allowlist | 194 | 81 |
| eventsource | 173 | 60 |
| uievents | 163 | 14 |
| webxr | 163 | 162 |
| ai | 159 | 159 |
| scheduler | 144 | 36 |
| cookies | 134 | 61 |
| client-hints | 131 | 129 |
| permissions-policy | 122 | 84 |
| domxpath | 117 | 34 |
| touch-events | 113 | 5 |
| layout-instability | 110 | 76 |
| media-capabilities | 108 | 7 |
| compat | 107 | 9 |
| mediasession | 106 | 8 |
| fs | 101 | 43 |
| largest-contentful-paint | 100 | 69 |
| performance-timeline | 99 | 48 |
| encrypted-media | 98 | 98 |
| webtransport | 98 | 32 |
| infrastructure | 94 | 27 |
| webstorage | 94 | 38 |
| web-locks | 86 | 30 |
| input-events | 83 | 3 |
| paint-timing | 77 | 55 |
| signed-exchange | 77 | 57 |
| cookiestore | 76 | 56 |
| encoding-detection | 75 | 75 |
| wai-aria | 73 | 2 |
| resize-observer | 71 | 27 |
| webrtc-svc | 67 | 5 |
| import-maps | 63 | 22 |
| webusb | 62 | 32 |
| webrtc-extensions | 61 | 10 |
| gamepad | 58 | 6 |
| webmcp | 57 | 57 |
| fullscreen | 56 | 18 |
| longtask-timing | 56 | 21 |
| storage | 56 | 27 |
| entries-api | 55 | 3 |
| event-timing | 54 | 3 |
| permissions | 54 | 7 |
| loading | 53 | 53 |
| acid | 52 | 1 |
| element-timing | 52 | 52 |
| inert | 50 | 13 |
| storage-access-api | 50 | 39 |
| compute-pressure | 45 | 25 |
| focus | 43 | 33 |
| mediacapture-fromelement | 43 | 11 |
| js | 41 | 12 |
| long-animation-frame | 38 | 38 |
| beacon | 35 | 16 |
| remote-playback | 35 | 9 |
| animation-worklet | 34 | 23 |
| hr-time | 34 | 13 |
| reporting | 33 | 14 |
| requestidlecallback | 32 | 13 |
| web-bundle | 28 | 28 |
| container-timing | 27 | 27 |
| js-self-profiling | 27 | 25 |
| shape-detection | 27 | 24 |
| speech-api | 27 | 12 |
| device-bound-session-credentials | 26 | 26 |
| forced-colors-mode | 26 | 10 |
| scroll-to-text-fragment | 26 | 7 |
| console | 25 | 7 |
| webrtc-encoded-transform | 24 | 9 |
| screen-orientation | 23 | 2 |
| notifications | 22 | 15 |
| worklets | 22 | 21 |
| payment-request | 21 | 21 |
| video-rvfc | 21 | 8 |
| imagebitmap-renderingcontext | 20 | 14 |
| secure-contexts | 20 | 8 |
| clear-site-data | 19 | 9 |
| media-playback-quality | 19 | 1 |
| pointerlock | 19 | 3 |
| audio-session | 18 | 3 |
| close-watcher | 18 | 2 |
| document-policy | 18 | 10 |
| mediacapture-image | 18 | 15 |
| mst-content-hint | 18 | 4 |
| battery-status | 16 | 16 |
| measure-memory | 16 | 16 |
| visual-viewport | 16 | 10 |
| credential-management | 15 | 15 |
| push-api | 15 | 8 |
| x-frame-options | 15 | 4 |
| background-fetch | 14 | 11 |
| jpegxl | 14 | 10 |
| page-lifecycle | 14 | 5 |
| input-device-capabilities | 13 | 1 |
| mediacapture-streams | 13 | 12 |
| presentation-api | 13 | 13 |
| is-input-pending | 12 | 1 |
| mediacapture-insertable-streams | 12 | 12 |
| netinfo | 12 | 1 |
| network-error-logging | 12 | 12 |
| vibration | 12 | 3 |
| web-based-payment-handler | 12 | 9 |
| webrtc-stats | 12 | 7 |
| orientation-sensor | 11 | 11 |
| page-visibility | 11 | 6 |
| serial | 11 | 10 |
| web-nfc | 11 | 11 |
| file-system-access | 10 | 5 |
| geolocation | 10 | 7 |
| server-timing | 10 | 9 |
| web-share | 10 | 7 |
| autoplay-policy-detection | 9 | 3 |
| old-tests | 9 | 9 |
| payment-method-manifest | 9 | 9 |
| webrtc-priority | 9 | 2 |
| badging | 8 | 5 |
| captured-mouse-events | 8 | 4 |
| delegated-ink | 8 | 2 |
| digital-credentials | 8 | 8 |
| html-media-capture | 8 | 2 |
| keyboard-map | 8 | 8 |
| timing-entrytypes-registry | 8 | 2 |
| accelerometer | 7 | 7 |
| ambient-light | 7 | 7 |
| font-access | 7 | 7 |
| gyroscope | 7 | 7 |
| https-upgrades | 7 | 6 |
| keyboard-lock | 7 | 7 |
| magnetometer | 7 | 7 |
| orientation-event | 7 | 7 |
| png | 7 | 4 |
| soft-navigation-heuristics | 7 | 4 |
| webauthn | 7 | 7 |
| clipboard-apis | 6 | 6 |
| contenteditable | 6 | 2 |
| fenced-frame | 6 | 6 |
| html-aam | 6 | 1 |
| screen-wake-lock | 6 | 6 |
| virtual-keyboard | 6 | 3 |
| web-extensions | 6 | 6 |
| webhid | 6 | 5 |
| audio-output | 5 | 5 |
| content-index | 5 | 2 |
| density-size-correction | 5 | 2 |
| gpc | 5 | 1 |
| idle-detection | 5 | 5 |
| screen-capture | 5 | 5 |
| device-memory | 4 | 2 |
| payment-method-basic-card | 4 | 4 |
| permissions-request | 4 | 1 |
| permissions-revoke | 4 | 1 |
| ua-client-hints | 4 | 2 |
| webrtc-identity | 4 | 4 |
| bluetooth | 3 | 3 |
| direct-sockets | 3 | 3 |
| mediacapture-extensions | 3 | 3 |
| merchant-validation | 3 | 3 |
| periodic-background-sync | 3 | 2 |
| proximity | 3 | 3 |
| screen-details | 3 | 3 |
| trust-tokens | 3 | 3 |
| web-install | 3 | 3 |
| background-sync | 2 | 1 |
| generic-sensor | 2 | 2 |
| hsts | 2 | 1 |
| installedapp | 2 | 2 |
| managed | 2 | 2 |
| picture-in-picture | 2 | 2 |
| private-click-measurement | 2 | 1 |
| savedata | 2 | 1 |
| scroll-performance-timing | 2 | 1 |
| secure-payment-confirmation | 2 | 2 |
| subapps | 2 | 2 |
| apng | 1 | 1 |
| contacts | 1 | 1 |
| content-dpr | 1 | 1 |
| cpu-performance | 1 | 1 |
| deprecation-reporting | 1 | 1 |
| device-posture | 1 | 1 |
| document-picture-in-picture | 1 | 1 |
| ecmascript | 1 | 1 |
| eyedropper | 1 | 1 |
| geolocation-sensor | 1 | 1 |
| intervention-reporting | 1 | 1 |
| mediacapture-handle | 1 | 1 |
| mediacapture-region | 1 | 1 |
| payment-method-id | 1 | 1 |
| viewport | 1 | 1 |
| web-otp | 1 | 1 |
| webdriver | 1 | 1 |
| webmidi | 1 | 1 |
| webrtc-ice | 1 | 1 |
| window-management | 1 | 1 |

## Suspected fix areas (ranked by failing-subtest count)

- unclassified (274556 failing subtests)
- missing/incomplete JS API (bootstrap.js) (12452 failing subtests)
- obscura-js event loop (8000 failing subtests)
- obscura-dom (tree.rs / bootstrap.js) (5855 failing subtests)
- obscura-net / bootstrap.js fetch shim (4220 failing subtests)
- css (obscura-dom style / bootstrap.js) (667 failing subtests)
- events (bootstrap.js) (61 failing subtests)
- storage (bootstrap.js / obscura-net cookie jar) (6 failing subtests)
