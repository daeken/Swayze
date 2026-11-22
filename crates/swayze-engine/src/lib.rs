//! Host-facing Swayze API. Only the documented language subsets are supported.

pub use swayze_js::{
    Error as JsParseError, ErrorKind as JsErrorKind, EvalError as JsEvalError, Limit as JsLimit,
    Limits as JsLimits, Script, Span as JsSpan, parse_script, parse_script_with_limits,
};
pub use swayze_wasm::{
    Error as WasmValidationError, ErrorKind as WasmErrorKind, Instance as WasmInstance,
    IntegerField as WasmIntegerField, InvokeError as WasmInvokeError,
    LengthField as WasmLengthField, Limit as WasmLimit, Module as WasmModule, Trap as WasmTrap,
    validate_module,
};
