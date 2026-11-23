//! Host-facing Swayze API. Only the documented language subsets are supported.
//!
//! [`JsPrimitiveValue::String`] borrows exact UTF-16 code units from a
//! prepared [`Script`], including any unpaired surrogates.
//!
//! [`WasmValue::F32`] and [`WasmValue::F64`] carry exact IEEE 754 bits,
//! including signed zeros and all NaN encodings, rather than Rust floats.

pub use swayze_js::{
    CompletionScript as JsCompletionScript, Error as JsParseError, ErrorKind as JsErrorKind,
    EvalError as JsEvalError, EvalScratch as JsEvalScratch, Limit as JsLimit, Limits as JsLimits,
    ParseErrorType as JsParseErrorType, PrimitiveValue as JsPrimitiveValue, Script, Span as JsSpan,
    Value as JsValue, parse_script, parse_script_for_completion,
    parse_script_for_completion_with_limits, parse_script_with_limits,
};
pub use swayze_wasm::{
    Error as WasmValidationError, ErrorKind as WasmErrorKind, ExecutionLimit as WasmExecutionLimit,
    GlobalImport as WasmGlobalImport, GlobalImportBinding as WasmGlobalImportBinding,
    GlobalImportInstantiationError as WasmGlobalImportInstantiationError, Instance as WasmInstance,
    InstantiationError as WasmInstantiationError, IntegerField as WasmIntegerField,
    InvokeError as WasmInvokeError, LengthField as WasmLengthField, Limit as WasmLimit,
    Module as WasmModule, ModuleWithGlobalImports as WasmModuleWithGlobalImports,
    ReadGlobalError as WasmReadGlobalError, ReadMemoryError as WasmReadMemoryError,
    ResultKind as WasmResultKind, TableSizeError as WasmTableSizeError, Trap as WasmTrap,
    ValType as WasmValType, Value as WasmValue, validate_module,
    validate_module_with_global_imports,
};
