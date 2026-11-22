#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = aeon_gguf::parse(data);
    let _ = aeon_gguf::parse_header_only(data);
});
