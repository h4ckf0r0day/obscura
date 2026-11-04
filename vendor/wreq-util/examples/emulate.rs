use wreq_util::{Emulation, Platform, Profile};

#[tokio::main]
async fn main() -> Result<(), wreq::Error> {
    // Example 1: Basic emulation - Safari26
    let text = wreq::get("https://tls.browserleaks.com/")
        .emulation(Emulation::Safari26)
        .send()
        .await?
        .text()
        .await?;
    println!("{}\n", text);

    // Example 2: Advanced emulation with options - OkHttp5
    let text = wreq::get("https://tls.peet.ws/api/all")
        .emulation(
            Emulation::builder()
                .profile(Profile::OkHttp5)
                .platform(Platform::Windows)
                .http2(false)
                .build(),
        )
        .send()
        .await?
        .text()
        .await?;
    println!("{}\n", text);

    // Example 3: Random device emulation
    let text = wreq::get("https://tls.peet.ws/api/all")
        .emulation(Emulation::random())
        .send()
        .await?
        .text()
        .await?;
    println!("{}", text);

    Ok(())
}
