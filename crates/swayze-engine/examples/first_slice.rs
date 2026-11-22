//! Run the first supported JavaScript and WebAssembly slices.

use swayze_engine::{parse_script, validate_module};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let script = parse_script("(2 + 3) * 4;")?;
    println!("JavaScript result: {}", script.evaluate()?);

    // Import-free `() -> i32` exported as `answer`, with body `i32.const 42`.
    let bytes = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0\
                  \x07\x0a\x01\x06answer\0\0\x0a\x06\x01\x04\0\x41\x2a\x0b";
    let module = validate_module(bytes)?;
    let result = module.instantiate().invoke_i32("answer")?;
    println!("WebAssembly result: {result}");
    Ok(())
}
