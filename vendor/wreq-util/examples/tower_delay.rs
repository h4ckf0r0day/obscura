use std::time::{Duration, Instant};
use wreq::Client;
use wreq_util::tower::delay::{DelayLayer, JitterDelayLayer};

#[tokio::main]
async fn main() -> Result<(), wreq::Error> {
    // Build a client with a 1 second delay between requests
    let client = Client::builder()
        .layer(DelayLayer::new(Duration::from_secs(1)))
        .tls_cert_verification(false)
        .build()?;

    let url = "https://httpbin.io/get";
    println!("Sending 3 requests to {} using DelayLayer(1s)", url);

    let mut prev_start = Instant::now();

    for i in 1..=3 {
        let start = Instant::now();
        let delta = start.duration_since(prev_start);
        println!("Request #{}, start (+{:.3}s)", i, delta.as_secs_f64());

        let _ = client.get(url).send().await?;

        println!(
            "Request #{}, completed in {:.3}s",
            i,
            start.elapsed().as_secs_f64()
        );

        prev_start = start;
    }

    // Example 1: Basic jittered delay for all requests
    basic_jitter_delay().await?;

    println!("\n{}\n", "=".repeat(60));

    // Example 2: Conditional jittered delay using `when`
    conditional_jitter_delay().await?;

    println!("\n{}\n", "=".repeat(60));

    // Example 3: Conditional fixed delay using `when`
    conditional_fixed_delay().await?;

    Ok(())
}

/// Demonstrates basic jittered delay aplied to all requests.
async fn basic_jitter_delay() -> Result<(), wreq::Error> {
    println!("=== Basic Jittered Delay ===\n");

    // Build a client with jittered delay: base 1s ± 50% (range: 0.5s to 1.5s)
    let client = Client::builder()
        .layer(JitterDelayLayer::new(Duration::from_secs(1), 0.5))
        .tls_cert_verification(false)
        .build()?;

    let url = "https://httpbin.io/get";
    println!(
        "Sending 3 requests to {} using JitterDelayLayer(1s ± 50%)",
        url
    );
    println!("Expected delay range: [0.5s, 1.5s]\n");

    let mut prev_start = Instant::now();

    for i in 1..=3 {
        let start = Instant::now();
        let delta = start.duration_since(prev_start);

        if i > 1 {
            println!(
                "Request #{}: delay since last = {:.3}s",
                i,
                delta.as_secs_f64()
            );
        } else {
            println!("Request #{}: starting...", i);
        }

        let _ = client.get(url).send().await?;

        println!(
            "Request #{}: completed in {:.3}s\n",
            i,
            start.elapsed().as_secs_f64()
        );

        prev_start = start;
    }

    println!("Notice how the delays vary randomly within the [0.5s, 1.5s] range.");

    Ok(())
}

/// Demonstrates conditional jittered delay using `when` predicate.
async fn conditional_jitter_delay() -> Result<(), wreq::Error> {
    println!("=== Conditional Jittered Delay (using `JitterDelayLayer::when`) ===\n");

    let client = Client::builder()
        .layer(JitterDelayLayer::new(Duration::from_millis(800), 0.3).when(
            |req: &http::Request<_>| {
                // Only apply delay to POST requests
                req.method() == http::Method::POST
            },
        ))
        .tls_cert_verification(false)
        .build()?;

    const BASE_URL: &str = "https://httpbin.io";

    println!("POST requests will have jittered delay (800ms ± 30%)");
    println!("GET requests will pass through immediately\n");

    // Send alternating GET and POST requests
    for i in 1..=4 {
        let start = Instant::now();

        if i % 2 == 1 {
            // GET request - no delay expected
            let url = format!("{BASE_URL}/get");
            println!("Request #{}: GET {}", i, url);
            let _ = client.get(&url).send().await?;
        } else {
            // POST request - delay expected
            let url = format!("{BASE_URL}/post");
            println!("Request #{}: POST {}", i, url);
            let _ = client.post(&url).send().await?;
        }

        println!(
            "Request #{}: completed in {:.3}s ({})\n",
            i,
            start.elapsed().as_secs_f64(),
            if i % 2 == 1 { "no delay" } else { "with delay" }
        );
    }

    println!("Notice: GET requests complete quickly, POST requests have ~800ms delay.");

    Ok(())
}

/// Demonstrates conditional fixed delay using `when` predicate.
async fn conditional_fixed_delay() -> Result<(), wreq::Error> {
    println!("=== Conditional Fixed Delay (using `DelayLayer::when`) ===\n");

    // Build a client that only delays requests to specific paths
    let client = Client::builder()
        .layer(
            DelayLayer::new(Duration::from_secs(1)).when(|req: &http::Request<_>| {
                // Only apply delay to /delay/* endpoints
                req.uri().path().starts_with("/delay")
            }),
        )
        .tls_cert_verification(false)
        .build()?;

    const BASE_URL: &str = "https://httpbin.io";

    println!("Requests to /delay/* will have 1s fixed delay");
    println!("Other requests will pass through immediately\n");

    // Test with different endpoints
    let endpoints = ["/get", "/delay/0"];

    for (i, endpoint) in endpoints.iter().enumerate() {
        let url = format!("{BASE_URL}{endpoint}");
        let start = Instant::now();

        println!("Request #{}: GET {}", i + 1, url);
        let _ = client.get(&url).send().await?;

        let elapsed = start.elapsed().as_secs_f64();
        let has_delay = endpoint.starts_with("/delay");

        println!(
            "Request #{}: completed in {:.3}s ({})\n",
            i + 1,
            elapsed,
            if has_delay { "with delay" } else { "no delay" }
        );
    }

    println!("Notice: /get completes quickly, /delay/* has additional 1s delay.");

    Ok(())
}
