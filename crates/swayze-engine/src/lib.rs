//! Host-facing Swayze API. Only the documented language subsets are supported.

pub use swayze_js::{
    Error as JsParseError, ErrorKind as JsErrorKind, EvalError as JsEvalError, Limit as JsLimit,
    Limits as JsLimits, Script, Span as JsSpan, Value as JsValue, parse_script,
    parse_script_with_limits,
};
pub use swayze_wasm::{
    Error as WasmValidationError, ErrorKind as WasmErrorKind, ExecutionLimit as WasmExecutionLimit,
    Instance as WasmInstance, InstantiationError as WasmInstantiationError,
    IntegerField as WasmIntegerField, InvokeError as WasmInvokeError,
    LengthField as WasmLengthField, Limit as WasmLimit, Module as WasmModule,
    ResultKind as WasmResultKind, Trap as WasmTrap, ValType as WasmValType, Value as WasmValue,
    validate_module,
};
