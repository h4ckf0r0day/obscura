use crate::runtime::ObscuraJsRuntime;
use serde_json::{json, Value};

async fn probe(script: &str) -> Value {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<!doctype html><body></body>"));
    rt.set_url("https://webcrypto.test/");
    rt.run_page_init();
    rt.execute_script("webcrypto-regression", &format!(r#"
        globalThis.cryptoResult = null;
        const s = crypto.subtle;
        const hex = buf => Array.from(new Uint8Array(buf), b => b.toString(16).padStart(2, '0')).join('');
        const fromHex = h => new Uint8Array(h.match(/../g).map(b => parseInt(b, 16)));
        const errorName = async fn => {{ try {{ await fn(); return null; }} catch (e) {{ return e.name; }} }};
        (async () => {{ {script} }})().then(
            value => cryptoResult = value,
            error => cryptoResult = {{error:error.name, message:error.message}}
        );
    "#)).unwrap();
    rt.run_event_loop_bounded(1000).await.unwrap();
    rt.evaluate("cryptoResult").unwrap()
}

#[tokio::test]
async fn aes_kw_wraps_and_unwraps_raw_keys() {
    // RFC 3394, section 4.1, through wrapKey / unwrapKey.
    let out = probe(r#"
        const kek = await s.importKey('raw', fromHex('000102030405060708090a0b0c0d0e0f'), 'AES-KW', false, ['wrapKey', 'unwrapKey']);
        const key = await s.importKey('raw', fromHex('00112233445566778899aabbccddeeff'), 'AES-CBC', true, ['encrypt']);
        const wrapped = await s.wrapKey('raw', key, kek, 'AES-KW');
        const back = await s.unwrapKey('raw', wrapped, kek, {name: 'AES-KW'}, 'AES-GCM', true, ['encrypt']);
        const tampered = new Uint8Array(wrapped.slice(0)); tampered[0] ^= 1;
        const generated = await s.generateKey({name: 'AES-KW', length: 256}, false, ['wrapKey', 'unwrapKey']);
        const roundTrip = await s.unwrapKey('raw', await s.wrapKey('raw', key, generated, 'AES-KW'), generated, 'AES-KW', 'AES-CBC', true, ['decrypt']);
        // RFC 3394 wraps at least two blocks, of 8 bytes each.
        const hmac8 = await s.importKey('raw', new Uint8Array(8).fill(1), {name: 'HMAC', hash: 'SHA-256'}, true, ['sign']);
        const hmac3 = await s.importKey('raw', new Uint8Array([1, 2, 3]), {name: 'HMAC', hash: 'SHA-256'}, true, ['sign', 'verify']);
        const hmac16 = await s.importKey('raw', new Uint8Array(16).fill(2), {name: 'HMAC', hash: 'SHA-256'}, true, ['sign']);
        const hmac16Back = await s.unwrapKey('raw', await s.wrapKey('raw', hmac16, kek, 'AES-KW'), kek, 'AES-KW', {name: 'HMAC', hash: 'SHA-256'}, true, ['sign']);
        return [
            hex(wrapped),
            back.algorithm.name, hex(await s.exportKey('raw', back)),
            hex(await s.exportKey('raw', roundTrip)),
            await errorName(() => s.unwrapKey('raw', tampered, kek, 'AES-KW', 'AES-GCM', true, ['encrypt'])),
            await errorName(() => s.encrypt('AES-KW', kek, new Uint8Array(16))),
            await errorName(() => s.wrapKey('raw', hmac8, kek, 'AES-KW')),
            // This JWK's JSON text is 77 bytes, not a multiple of 8.
            await errorName(() => s.wrapKey('jwk', hmac3, kek, 'AES-KW')),
            await errorName(() => s.unwrapKey('raw', new Uint8Array(16), kek, 'AES-KW', 'AES-GCM', true, ['encrypt'])),
            hmac16Back.algorithm.length,
        ];
    "#).await;
    assert_eq!(out, json!([
        "1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5",
        "AES-GCM", "00112233445566778899aabbccddeeff",
        "00112233445566778899aabbccddeeff",
        "OperationError",
        "NotSupportedError",
        "OperationError",
        "OperationError",
        "OperationError",
        128,
    ]));
}

#[tokio::test]
async fn wrapped_jwks_ignore_the_pages_json() {
    let out = probe(r#"
        const gcm = {name: 'AES-GCM', iv: new Uint8Array(12)};
        const wrapper = await s.generateKey({name: 'AES-GCM', length: 128}, true, ['wrapKey', 'unwrapKey', 'decrypt', 'encrypt']);
        const hmac = await s.importKey('raw', new Uint8Array([1, 2, 3]), {name: 'HMAC', hash: 'SHA-256'}, true, ['sign', 'verify']);
        Object.prototype.toJSON = () => ({injected: true});
        Array.prototype.toJSON = () => 'injected';
        const realParse = JSON.parse;
        JSON.parse = () => ({kty: 'oct', k: 'AAAA'});
        let text, unwrapped, sig;
        try {
            const wrapped = await s.wrapKey('jwk', hmac, wrapper, gcm);
            text = new TextDecoder().decode(await s.decrypt(gcm, wrapper, wrapped));
            unwrapped = await s.unwrapKey('jwk', wrapped, wrapper, gcm, {name: 'HMAC', hash: 'SHA-256'}, true, ['sign']);
        } finally {
            delete Object.prototype.toJSON;
            delete Array.prototype.toJSON;
            JSON.parse = realParse;
        }
        const notJson = await s.encrypt(gcm, wrapper, new TextEncoder().encode('not json'));
        return [
            text,
            JSON.stringify(await s.exportKey('jwk', hmac)) === text,
            hex(await s.exportKey('raw', unwrapped)),
            await errorName(() => s.unwrapKey('jwk', notJson, wrapper, gcm, {name: 'HMAC', hash: 'SHA-256'}, true, ['sign'])),
        ];
    "#).await;
    assert_eq!(out, json!([
        r#"{"alg":"HS256","ext":true,"k":"AQID","key_ops":["sign","verify"],"kty":"oct"}"#,
        true,
        "010203",
        "DataError",
    ]));
}

#[tokio::test]
async fn key_wrapping_checks_the_wrapping_keys_algorithm_and_usage() {
    let out = probe(r#"
        const bytes = new Uint8Array(16);
        const payload = await s.importKey('raw', bytes, 'AES-GCM', true, ['encrypt']);
        const results = [];
        for (const name of ['AES-KW', 'AES-GCM', 'AES-CBC', 'AES-CTR']) {
            const alg = name === 'AES-GCM' ? {name, iv: new Uint8Array(12)}
                : name === 'AES-CBC' ? {name, iv: new Uint8Array(16)}
                : name === 'AES-CTR' ? {name, counter: new Uint8Array(16), length: 64} : name;
            const unwrapOnly = await s.importKey('raw', bytes, name, false, ['unwrapKey']);
            const wrongAlgorithm = await s.importKey('raw', bytes, {name:'HMAC', hash:'SHA-256'}, false, ['sign']);
            results.push(await errorName(() => s.wrapKey('raw', payload, unwrapOnly, alg)));
            results.push(await errorName(() => s.wrapKey('raw', payload, wrongAlgorithm, alg)));
            wrongAlgorithm.algorithm.name = name;
            results.push(await errorName(() => s.wrapKey('raw', payload, wrongAlgorithm, alg)));
            const wrapOnly = await s.importKey('raw', bytes, name, false, ['wrapKey']);
            wrapOnly.algorithm.name = 'HMAC';
            results.push((await s.wrapKey('raw', payload, wrapOnly, alg)).byteLength > 0);
        }
        return results;
    "#).await;
    assert_eq!(out, json!([
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", true,
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", true,
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", true,
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", true,
    ]));
}

#[tokio::test]
async fn key_unwrapping_checks_the_unwrapping_keys_algorithm_and_usage() {
    let out = probe(r#"
        const bytes = new Uint8Array(16);
        const payload = await s.importKey('raw', bytes, 'AES-GCM', true, ['encrypt']);
        const results = [];
        for (const name of ['AES-KW', 'AES-GCM', 'AES-CBC', 'AES-CTR']) {
            const alg = name === 'AES-GCM' ? {name, iv: new Uint8Array(12)}
                : name === 'AES-CBC' ? {name, iv: new Uint8Array(16)}
                : name === 'AES-CTR' ? {name, counter: new Uint8Array(16), length: 64} : name;
            const wrapOnly = await s.importKey('raw', bytes, name, false, ['wrapKey']);
            const wrapped = await s.wrapKey('raw', payload, wrapOnly, alg);
            const wrongAlgorithm = await s.importKey('raw', bytes, {name:'HMAC', hash:'SHA-256'}, false, ['sign']);
            const unwrap = key => s.unwrapKey('raw', wrapped, key, alg, 'AES-GCM', true, ['encrypt']);
            results.push(await errorName(() => unwrap(wrapOnly)));
            results.push(await errorName(() => unwrap(wrongAlgorithm)));
            wrongAlgorithm.algorithm.name = name;
            results.push(await errorName(() => unwrap(wrongAlgorithm)));
            const unwrapOnly = await s.importKey('raw', bytes, name, false, ['unwrapKey']);
            unwrapOnly.algorithm.name = 'HMAC';
            const restored = await unwrap(unwrapOnly);
            results.push(hex(await s.exportKey('raw', restored)));
        }
        return results;
    "#).await;
    assert_eq!(out, json!([
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", "00000000000000000000000000000000",
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", "00000000000000000000000000000000",
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", "00000000000000000000000000000000",
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", "00000000000000000000000000000000",
    ]));
}

#[tokio::test]
async fn unsupported_wrapping_algorithms_still_report_not_supported() {
    let out = probe(r#"
        const key = await s.importKey('raw', new Uint8Array(16), {name:'HMAC', hash:'SHA-256'}, true, ['sign']);
        return [
            await errorName(() => s.wrapKey('raw', key, key, 'HMAC')),
            await errorName(() => s.unwrapKey('raw', new Uint8Array(24), key, 'HMAC', 'AES-GCM', false, ['encrypt'])),
        ];
    "#).await;
    assert_eq!(out, json!(["NotSupportedError", "NotSupportedError"]));
}

#[tokio::test]
async fn cloned_wrapping_keys_keep_their_internal_algorithm() {
    let out = probe(r#"
        const key = await s.importKey('raw', new Uint8Array(16), 'AES-KW', false, ['wrapKey', 'unwrapKey']);
        key.algorithm.name = 'HMAC';
        const copy = structuredClone({first:key, second:key});
        const payload = await s.importKey('raw', new Uint8Array(16), 'AES-GCM', true, ['encrypt']);
        const wrapped = await s.wrapKey('raw', payload, copy.first, 'AES-KW');
        const restored = await s.unwrapKey('raw', wrapped, copy.first, 'AES-KW', 'AES-GCM', true, ['encrypt']);
        return [copy.first.algorithm.name, copy.first === copy.second, hex(await s.exportKey('raw', restored))];
    "#).await;
    assert_eq!(out, json!(["AES-KW", true, "00000000000000000000000000000000"]));
}
