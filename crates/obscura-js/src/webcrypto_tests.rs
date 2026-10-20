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

// RFC 6979, appendix A.2.5: a P-256 key and its deterministic SHA-256 signature
// of "sample", which is also what this implementation's RFC 6979 nonce gives.
const RFC6979_P256: &str = r#"
    const b64url = bytes => btoa(String.fromCharCode(...bytes)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
    const P256 = {name: 'ECDSA', namedCurve: 'P-256'};
    const ES256 = {name: 'ECDSA', hash: 'SHA-256'};
    const rfcPublic = {kty: 'EC', crv: 'P-256',
        x: b64url(fromHex('60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6')),
        y: b64url(fromHex('7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299'))};
    const rfcPrivate = {...rfcPublic, d: b64url(fromHex('c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721'))};
    const rfcSignature = 'efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716'
        + 'f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8';
    const sample = new TextEncoder().encode('sample');
"#;

const RFC6979_P256_SIGNATURE: &str = "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716\
                                      f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8";

#[tokio::test]
async fn ecdsa_p256_signs_and_verifies() {
    let out = probe(&format!("{RFC6979_P256}{}", r#"
        // A DPoP-style key: generated non-extractable, its public JWK exported.
        const pair = await s.generateKey({name: 'ECDSA', namedCurve: 'P-256', hash: {name: 'SHA-256'}}, false, ['sign', 'verify']);
        const jwk = await s.exportKey('jwk', pair.publicKey);
        const signature = await s.sign(ES256, pair.privateKey, sample);
        const known = await s.importKey('jwk', rfcPrivate, P256, false, ['sign']);
        const knownPublic = await s.importKey('jwk', rfcPublic, P256, false, ['verify']);
        const tampered = fromHex(rfcSignature); tampered[63] ^= 1;
        const sha512 = await s.sign({name: 'ECDSA', hash: 'SHA-512'}, known, sample);
        return [
            JSON.stringify(pair.publicKey.algorithm), pair.publicKey.type, pair.publicKey.extractable, pair.publicKey.usages,
            JSON.stringify(pair.privateKey.algorithm), pair.privateKey.type, pair.privateKey.extractable, pair.privateKey.usages,
            Object.keys(jwk), jwk.x.length, jwk.y.length,
            signature.byteLength,
            await s.verify(ES256, pair.publicKey, signature, sample),
            await s.verify(ES256, pair.publicKey, signature, new Uint8Array(3)),
            hex(await s.sign(ES256, known, sample)),
            await s.verify(ES256, knownPublic, fromHex(rfcSignature), sample),
            await s.verify(ES256, knownPublic, tampered, sample),
            await s.verify(ES256, knownPublic, fromHex(rfcSignature).slice(1), sample),
            await s.verify(ES256, knownPublic, new Uint8Array(64), sample),
            await s.verify({name: 'ECDSA', hash: 'SHA-512'}, knownPublic, sha512, sample),
            await s.verify(ES256, knownPublic, sha512, sample),
        ];
    "#)).await;
    assert_eq!(out, json!([
        r#"{"name":"ECDSA","namedCurve":"P-256"}"#, "public", true, ["verify"],
        r#"{"name":"ECDSA","namedCurve":"P-256"}"#, "private", false, ["sign"],
        ["crv", "ext", "key_ops", "kty", "x", "y"], 43, 43,
        64,
        true,
        false,
        RFC6979_P256_SIGNATURE,
        true,
        false,
        false,
        false,
        true,
        false,
    ]));
}

#[tokio::test]
async fn ecdsa_p256_keys_import_and_export() {
    let out = probe(&format!("{RFC6979_P256}{}", r#"
        const privateKey = await s.importKey('jwk', rfcPrivate, P256, true, ['sign']);
        const publicKey = await s.importKey('jwk', rfcPublic, P256, true, ['verify']);
        const raw = new Uint8Array(await s.exportKey('raw', publicKey));
        const spki = await s.exportKey('spki', publicKey);
        const pkcs8 = await s.exportKey('pkcs8', privateKey);
        const compressed = new Uint8Array([0x02 | (raw[64] & 1), ...raw.slice(1, 33)]);
        const fromRaw = await s.importKey('raw', compressed, P256, true, ['verify']);
        const fromSpki = await s.importKey('spki', spki, P256, true, ['verify']);
        const fromPkcs8 = await s.importKey('pkcs8', pkcs8, P256, true, ['sign']);
        // A PKCS#8 key without its public key gets it derived.
        const noPublic = fromHex('3041020100301306072a8648ce3d020106082a8648ce3d03010704273025020101'
            + '0420c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721');
        const fromBareScalar = await s.importKey('pkcs8', noPublic, P256, true, ['sign']);
        // wrapKey/unwrapKey a private JWK, and a structured clone of the key.
        const gcm = {name: 'AES-GCM', iv: new Uint8Array(12)};
        const wrapper = await s.generateKey({name: 'AES-GCM', length: 128}, false, ['wrapKey', 'unwrapKey', 'decrypt']);
        const wrapped = await s.wrapKey('jwk', privateKey, wrapper, gcm);
        const unwrapped = await s.unwrapKey('jwk', wrapped, wrapper, gcm, P256, false, ['sign']);
        const clone = structuredClone(privateKey);
        const expectedJwk = {crv: 'P-256', d: rfcPrivate.d, ext: true, key_ops: ['sign'], kty: 'EC', x: rfcPublic.x, y: rfcPublic.y};
        return [
            hex(raw).slice(0, 4), raw.length,
            hex(spki).slice(0, 54), spki.byteLength,
            hex(pkcs8).slice(0, 72), pkcs8.byteLength,
            hex(await s.exportKey('raw', fromRaw)) === hex(raw),
            hex(await s.exportKey('raw', fromSpki)) === hex(raw),
            hex(await s.exportKey('pkcs8', fromPkcs8)) === hex(pkcs8),
            hex(await s.exportKey('pkcs8', fromBareScalar)) === hex(pkcs8),
            JSON.stringify(await s.exportKey('jwk', privateKey)) === JSON.stringify(expectedJwk),
            new TextDecoder().decode(await s.decrypt(gcm, wrapper, wrapped)) === JSON.stringify(expectedJwk),
            hex(await s.sign(ES256, unwrapped, sample)),
            hex(await s.sign(ES256, clone, sample)),
            (await s.importKey('raw', raw, P256, true, [])).usages,
            (await s.importKey('jwk', {...rfcPublic, alg: 'ES256', use: 'sig', key_ops: ['verify'], ext: true}, P256, true, ['verify'])).type,
        ];
    "#)).await;
    assert_eq!(out, json!([
        "0460", 65,
        "3059301306072a8648ce3d020106082a8648ce3d03010703420004", 91,
        "308187020100301306072a8648ce3d020106082a8648ce3d030107046d306b0201010420", 138,
        true, true, true, true, true, true,
        RFC6979_P256_SIGNATURE, RFC6979_P256_SIGNATURE,
        [],
        "public",
    ]));
}

#[tokio::test]
async fn ecdsa_p256_reports_the_specs_errors() {
    let out = probe(&format!("{RFC6979_P256}{}", r#"
        const pair = await s.generateKey(P256, true, ['sign', 'verify']);
        const raw = new Uint8Array(await s.exportKey('raw', pair.publicKey));
        const pkcs8 = await s.exportKey('pkcs8', pair.privateKey);
        const offCurve = raw.slice(); offCurve[64] ^= 1;
        const spki = new Uint8Array(await s.exportKey('spki', pair.publicKey));
        const d = '01'.repeat(32);
        const prefix = '020100301306072a8648ce3d020106082a8648ce3d030107';
        const p384Params = fromHex('304a' + prefix + '0430302e0201010420' + d + 'a00706052b81040022');
        const otherPublic = fromHex('308187' + prefix + '046d306b0201010420' + d + 'a144034200' + hex(raw));
        const notExtractable = await s.generateKey(P256, false, ['sign']);
        const generate = (alg, usages) => errorName(() => s.generateKey(alg, true, usages));
        const importJwk = (jwk, usages, extractable = true) => errorName(() => s.importKey('jwk', jwk, P256, extractable, usages));
        return [
            // generateKey: usages are checked before the curve.
            await generate(P256, []),
            await generate(P256, ['verify']),
            await generate(P256, ['encrypt']),
            await generate({name: 'ECDSA', namedCurve: 'X'}, ['encrypt']),
            await generate({name: 'ECDSA', namedCurve: 'p-256'}, ['sign']),
            await generate({name: 'ECDSA', namedCurve: 'P-384'}, ['sign']),
            await generate({name: 'ECDSA'}, ['sign']),
            await generate(P256, ['sign', 'nonsense']),
            await generate(P256, ['sign', 'encapsulateKey']),
            (await s.generateKey(P256, true, ['verify', 'sign', 'sign'])).privateKey.usages,
            // sign / verify
            await errorName(() => s.sign({name: 'ECDSA'}, pair.privateKey, sample)),
            await errorName(() => s.sign({name: 'ECDSA', hash: 'MD5'}, pair.privateKey, sample)),
            await errorName(() => s.sign(ES256, pair.publicKey, sample)),
            await errorName(() => s.verify(ES256, pair.privateKey, new Uint8Array(64), sample)),
            // raw / spki / pkcs8
            await errorName(() => s.importKey('raw', raw, P256, true, ['sign'])),
            await errorName(() => s.importKey('raw', raw.slice(1), P256, true, ['verify'])),
            await errorName(() => s.importKey('raw', new Uint8Array([0]), P256, true, ['verify'])),
            await errorName(() => s.importKey('raw', offCurve, P256, true, ['verify'])),
            // P-384 isn't supported yet; an SPKI naming it is another curve's key.
            await errorName(() => s.importKey('spki', spki, {name: 'ECDSA', namedCurve: 'P-384'}, true, ['verify'])),
            await errorName(() => s.importKey('spki', fromHex('3056301006072a8648ce3d020106052b81040022' + hex(spki.slice(23))), P256, true, ['verify'])),
            await errorName(() => s.importKey('spki', new Uint8Array([...spki, 0]), P256, true, ['verify'])),
            await errorName(() => s.importKey('spki', pkcs8, P256, true, ['verify'])),
            await errorName(() => s.importKey('pkcs8', pkcs8, P256, true, [])),
            await errorName(() => s.importKey('pkcs8', pkcs8, P256, true, ['verify'])),
            await errorName(() => s.importKey('pkcs8', p384Params, P256, true, ['sign'])),
            await errorName(() => s.importKey('pkcs8', otherPublic, P256, true, ['sign'])),
            await errorName(() => s.importKey('pkcs8', pkcs8, {name: 'ECDSA'}, true, ['sign'])),
            await errorName(() => s.importKey('pkcs8', pkcs8, {name: 'ECDSA', namedCurve: 'P-521'}, true, ['sign'])),
            // jwk
            await importJwk(rfcPublic, ['sign']),
            await importJwk(rfcPrivate, ['verify']),
            await importJwk(rfcPrivate, []),
            await importJwk({...rfcPublic, kty: 'RSA'}, ['verify']),
            await importJwk({...rfcPublic, use: 'enc'}, ['verify']),
            await importJwk({...rfcPublic, use: 'enc'}, []),
            await importJwk({...rfcPublic, key_ops: ['sign']}, ['verify']),
            await importJwk({...rfcPublic, key_ops: ['verify', 'verify']}, ['verify']),
            await importJwk({...rfcPublic, ext: false}, ['verify']),
            await importJwk({...rfcPublic, ext: false}, ['verify'], false),
            await importJwk({...rfcPublic, crv: 'P-384'}, ['verify']),
            await importJwk({...rfcPublic, alg: 'ES384'}, ['verify']),
            await importJwk({...rfcPublic, alg: 'foo'}, ['verify']),
            await importJwk({...rfcPublic, x: rfcPublic.x.slice(1)}, ['verify']),
            await importJwk({...rfcPublic, x: rfcPublic.x + '='}, ['verify']),
            await importJwk({...rfcPublic, x: '!' + rfcPublic.x.slice(1)}, ['verify']),
            await importJwk({kty: 'EC', crv: 'P-256', x: rfcPublic.x}, ['verify']),
            await importJwk({...rfcPrivate, d: rfcPrivate.d.slice(2)}, ['sign']),
            await importJwk({...rfcPrivate, d: 'AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE'}, ['sign']),
            await errorName(() => s.importKey('jwk', 'not an object', P256, true, ['verify'])),
            await errorName(() => s.importKey('pem', raw, P256, true, ['verify'])),
            await errorName(() => s.importKey('raw', Array.from(raw), P256, true, ['verify'])),
            await errorName(() => s.importKey('spki', rfcPublic, P256, true, ['verify'])),
            // exportKey
            await errorName(() => s.exportKey('raw', pair.privateKey)),
            await errorName(() => s.exportKey('spki', pair.privateKey)),
            await errorName(() => s.exportKey('pkcs8', pair.publicKey)),
            await errorName(() => s.exportKey('jwk', notExtractable.privateKey)),
            notExtractable.publicKey.extractable,
        ];
    "#)).await;
    assert_eq!(out, json!([
        "SyntaxError", "SyntaxError", "SyntaxError", "SyntaxError", "NotSupportedError", "NotSupportedError", "TypeError", "TypeError", "SyntaxError",
        ["sign"],
        "TypeError", "NotSupportedError", "InvalidAccessError", "InvalidAccessError",
        "SyntaxError", "DataError", "DataError", "DataError", "NotSupportedError", "DataError", "DataError", "DataError",
        "SyntaxError", "SyntaxError", "DataError", "DataError", "TypeError", "NotSupportedError",
        "SyntaxError", "SyntaxError", "SyntaxError", "DataError", "DataError", null, "DataError", "DataError",
        "DataError", null, "DataError", "DataError", "DataError", "DataError", "DataError", "DataError", "DataError",
        "DataError", "DataError", "TypeError", "NotSupportedError", "TypeError", "TypeError",
        "InvalidAccessError", "InvalidAccessError", "InvalidAccessError", "InvalidAccessError",
        true,
    ]));
}
