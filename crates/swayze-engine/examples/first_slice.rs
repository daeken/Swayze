//! Run the first supported JavaScript and WebAssembly slices.

use swayze_engine::{parse_script, validate_module};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let script = parse_script("(2 + 3) * 4;")?;
    println!("JavaScript result: {}", script.evaluate()?);

    // `answer: (i32, i32) -> i32` with body `local.get 0; local.get 1; i32.sub`.
    let bytes = b"\0asm\x01\0\0\0\x01\x07\x01\x60\x02\x7f\x7f\x01\x7f\x03\x02\x01\0\
                  \x07\x0a\x01\x06answer\0\0\x0a\x09\x01\x07\0\x20\0\x20\x01\x6b\x0b";
    let module = validate_module(bytes)?;
    let result = module
        .instantiate()
        .invoke_i32_with_args("answer", &[45, 3])?;
    println!("WebAssembly result: {result}");
    Ok(())
}
