use swayze_engine::{Unsupported, prepare_js, prepare_wasm};

#[test]
fn javascript_is_unsupported_regardless_of_syntax() {
    for source in ["", "1 + 2;", "let = ;"] {
        assert_eq!(swayze_js::prepare(source), Err(swayze_js::Unsupported));
        assert_eq!(prepare_js(source), Err(Unsupported::JavaScript));
    }
}

#[test]
fn webassembly_is_unsupported_regardless_of_encoding() {
    // The eight-byte header alone encodes an empty version-1 module.
    for bytes in [&b""[..], b"\0asm\x01\0\0\0", b"\xff"] {
        assert_eq!(swayze_wasm::prepare(bytes), Err(swayze_wasm::Unsupported));
        assert_eq!(prepare_wasm(bytes), Err(Unsupported::WebAssembly));
    }
}
