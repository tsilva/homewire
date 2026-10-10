//! Administrative service entrypoint. Betty uses the standard Homewire adapter.
use homewire::{Result, computer, config::Source, device::Adapter, error};
use std::path::Path;
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err(error(
            "Usage: homewire-power keygen|serve|status|validate|shutdown PATH",
        ));
    }
    let path = Path::new(&args[1]);
    match args[0].as_str() {
        "keygen" => println!("{}", computer::generate_key(path)?),
        "serve" => computer::serve(path)?,
        "status" | "validate" | "shutdown" => {
            let client = computer::Manager::from_source(&Source::file(path))?;
            let value = match args[0].as_str() {
                "status" => client.status()?,
                "validate" => client.validate_shutdown()?,
                _ => client.shutdown()?,
            };
            println!("{value}");
        }
        _ => return Err(error("Unknown power service command")),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
