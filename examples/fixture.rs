//! Local healthy fixture server. Usage: `cargo run --example fixture [-- --port <n>]`.
//! See docs/fixture.md.

#[path = "../tests/support/fixture.rs"]
mod fixture;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut port: u16 = 8080;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let value = args.next().ok_or("--port needs a value")?;
                port = value.parse().map_err(|_| "--port must be 0-65535")?;
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let running = fixture::Fixture::start(port).await?;
    println!("fixture listening on http://{}", running.addr);
    println!("docs: docs/fixture.md (reset: POST /__admin/reset, stop: Ctrl-C)");
    tokio::signal::ctrl_c().await?;
    running.shutdown().await;
    Ok(())
}
