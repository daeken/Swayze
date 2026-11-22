//! Run the first supported JS slice and validate a WebAssembly module.

use swayze_engine::{parse_script, validate_module};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let script = parse_script("(2 + 3) * 4;")?;
    println!("JavaScript result: {}", script.evaluate()?);

    validate_module(b"\0asm\x01\0\0\0")?;
    println!("WebAssembly: validated an empty Core 1.0 module");
    Ok(())
}
