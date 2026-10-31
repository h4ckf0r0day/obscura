use std::sync::Arc;
use obscura_net::{CookieJar, ObscuraHttpClient};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Each fixed stream decodes to all byte values repeated four times.
// Generated independently with gzip, Brotli, and Zstandard encoders.
const GZIP: &[u8] = &[
    31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 99, 96, 100, 98, 102, 97, 101, 99, 231, 224,
    228, 226, 230, 225, 229, 227, 23, 16, 20, 18, 22, 17, 21, 19, 151, 144, 148, 146, 150, 145,
    149, 147, 87, 80, 84, 82, 86, 81, 85, 83, 215, 208, 212, 210, 214, 209, 213, 211, 55, 48,
    52, 50, 54, 49, 53, 51, 183, 176, 180, 178, 182, 177, 181, 179, 119, 112, 116, 114, 118, 113,
    117, 115, 247, 240, 244, 242, 246, 241, 245, 243, 15, 8, 12, 10, 14, 9, 13, 11, 143, 136,
    140, 138, 142, 137, 141, 139, 79, 72, 76, 74, 78, 73, 77, 75, 207, 200, 204, 202, 206, 201,
    205, 203, 47, 40, 44, 42, 46, 41, 45, 43, 175, 168, 172, 170, 174, 169, 173, 171, 111, 104,
    108, 106, 110, 105, 109, 107, 239, 232, 236, 234, 238, 233, 237, 235, 159, 48, 113, 210, 228, 41,
    83, 167, 77, 159, 49, 115, 214, 236, 57, 115, 231, 205, 95, 176, 112, 209, 226, 37, 75, 151,
    45, 95, 177, 114, 213, 234, 53, 107, 215, 173, 223, 176, 113, 211, 230, 45, 91, 183, 109, 223,
    177, 115, 215, 238, 61, 123, 247, 237, 63, 112, 240, 208, 225, 35, 71, 143, 29, 63, 113, 242,
    212, 233, 51, 103, 207, 157, 191, 112, 241, 210, 229, 43, 87, 175, 93, 191, 113, 243, 214, 237,
    59, 119, 239, 221, 127, 240, 240, 209, 227, 39, 79, 159, 61, 127, 241, 242, 213, 235, 55, 111,
    223, 189, 255, 240, 241, 211, 231, 47, 95, 191, 125, 255, 241, 243, 215, 239, 63, 127, 255, 253,
    103, 24, 245, 255, 168, 255, 71, 176, 255, 1, 38, 76, 11, 183, 0, 4, 0, 0,
];

const BROTLI: &[u8] = &[
    27, 255, 3, 0, 100, 64, 110, 150, 159, 151, 154, 76, 164, 33, 190, 52, 154, 224, 199, 5,
    0, 4, 129, 33, 80, 24, 124, 126, 12, 0, 225, 79, 46, 4, 18, 133, 198, 96, 113, 120,
    2, 145, 68, 166, 80, 105, 116, 6, 70, 38, 102, 22, 86, 54, 118, 14, 78, 46, 110, 30,
    94, 62, 126, 1, 65, 33, 97, 17, 81, 49, 113, 9, 73, 41, 105, 25, 89, 57, 121, 5,
    69, 37, 101, 21, 85, 53, 117, 13, 77, 45, 109, 29, 93, 61, 125, 3, 67, 35, 99, 19,
    83, 51, 115, 11, 75, 43, 107, 27, 91, 59, 123, 7, 71, 39, 103, 23, 87, 55, 119, 15,
    79, 47, 111, 31, 95, 63, 255, 128, 192, 160, 224, 144, 208, 176, 240, 136, 200, 168, 232, 152,
    216, 184, 248, 132, 196, 164, 228, 148, 212, 180, 244, 140, 204, 172, 236, 156, 220, 188, 252, 130,
    194, 162, 226, 146, 210, 178, 242, 138, 202, 170, 234, 154, 218, 186, 250, 134, 198, 166, 230, 150,
    214, 182, 246, 142, 206, 174, 238, 158, 222, 190, 254, 129, 193, 161, 225, 145, 209, 177, 241, 137,
    201, 169, 233, 153, 217, 185, 249, 133, 197, 165, 229, 149, 213, 181, 245, 141, 205, 173, 237, 157,
    221, 189, 253, 131, 195, 163, 227, 147, 211, 179, 243, 139, 203, 171, 235, 155, 219, 187, 251, 135,
    199, 167, 231, 151, 215, 183, 247, 143, 207, 175, 239, 159, 223, 191, 127, 64, 3,
];

#[cfg(feature = "stealth")]
const ZSTD: &[u8] = &[
    40, 181, 47, 253, 96, 0, 3, 85, 8, 0, 4, 16, 0, 1, 2, 3, 4, 5, 6, 7,
    8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47,
    48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67,
    68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87,
    88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107,
    108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127,
    128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147,
    148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166, 167,
    168, 169, 170, 171, 172, 173, 174, 175, 176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187,
    188, 189, 190, 191, 192, 193, 194, 195, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207,
    208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227,
    228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246, 247,
    248, 249, 250, 251, 252, 253, 254, 255, 1, 0, 0, 253, 6, 170, 53, 5,
];


async fn compressed_responses(stealth: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    #[allow(unused_mut)]
    let mut cases = vec![("gzip", GZIP), ("br", BROTLI)];
    #[cfg(feature = "stealth")]
    if stealth { cases.push(("zstd", ZSTD)); }
    #[cfg(not(feature = "stealth"))]
    { assert!(!stealth); }
    let requests = cases.iter().flat_map(|(encoding, bytes)| {
        [(*encoding, bytes.to_vec()), (*encoding, b"invalid compressed stream".to_vec()),
         (*encoding, bytes.to_vec())]
    }).collect::<Vec<_>>();
    let fixture = tokio::spawn(async move {
        for (encoding, bytes) in requests {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
            }
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Encoding: {encoding}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            ).as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    let client = ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true);
    #[cfg(feature = "stealth")]
    let stealth_client = obscura_net::StealthHttpClient::with_proxy(
        Arc::new(CookieJar::new()), None, true
    );
    let expected: Vec<u8> = (0..4).flat_map(|_| 0..=255u8).collect();
    for (encoding, _) in cases {
        for valid in [true, false, true] {
            #[cfg(feature = "stealth")]
            let response = if stealth { stealth_client.fetch(&url).await } else { client.fetch(&url).await };
            #[cfg(not(feature = "stealth"))]
            let response = client.fetch(&url).await;
            assert_eq!(client.active_requests(), 0, "{encoding}: native request leaked");
            #[cfg(feature = "stealth")]
            assert_eq!(stealth_client.active_requests(), 0, "{encoding}: stealth request leaked");
            if valid {
                let response = response.expect(encoding);
                assert_eq!(response.status, 200);
                assert_eq!(response.body, expected, "{encoding}: binary response changed");
            } else {
                assert!(response.is_err(), "{encoding}: invalid stream was accepted");
            }
        }
    }
    fixture.await.unwrap();
}

#[tokio::test]
async fn native_compressed_responses_recover_after_decode_errors() {
    tokio::time::timeout(std::time::Duration::from_secs(10), compressed_responses(false)).await.unwrap();
}

#[cfg(feature = "stealth")]
#[tokio::test]
async fn stealth_compressed_responses_recover_after_decode_errors() {
    tokio::time::timeout(std::time::Duration::from_secs(10), compressed_responses(true)).await.unwrap();
}

#[cfg(feature = "stealth")]
#[test]
fn tls_certificate_codecs_preserve_bytes_and_reject_corrupt_streams() {
    use wreq::tls::compress::{CertificateCompressor, Codec};
    use wreq_util::emulate::compress::{BrotliCompressor, ZstdCompressor};

    fn decode(codec: &Codec, input: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut output = Vec::new();
        match codec {
            Codec::Pointer(call) => call(input, &mut output)?,
            Codec::Dynamic(call) => call(input, &mut output)?,
        }
        Ok(output)
    }

    let expected: Vec<u8> = (0..4).flat_map(|_| 0..=255u8).collect();
    for (compressor, stream) in [
        (&BrotliCompressor as &dyn CertificateCompressor, BROTLI),
        (&ZstdCompressor as &dyn CertificateCompressor, ZSTD),
    ] {
        let decoder = compressor.decompress();
        assert_eq!(decode(&decoder, stream).unwrap(), expected);
        assert!(decode(&decoder, b"invalid compressed certificate").is_err());
        assert_eq!(decode(&decoder, stream).unwrap(), expected);
        let encoded = decode(&compressor.compress(), &expected).unwrap();
        assert_eq!(decode(&decoder, &encoded).unwrap(), expected);
    }
}
