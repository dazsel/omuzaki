//! Reading a model's signature from a loaded `ort` session, and building sessions: the session options that keep the
//! runtime quiet (one inter-op thread, no spinning) and the provider chain that ends on the CPU.

use omuzaki_ai_api::{DType, Dim, LoadError, Provider, SessionOptions, Signature, TensorSpec};
use ort::ep::{ExecutionProvider, ExecutionProviderDispatch};
use ort::session::Session;
use ort::session::builder::SessionBuilder;
use ort::value::{Outlet, TensorElementType, ValueType};

/// A provider that was asked for and not used, and why: shown to people when a model runs on the CPU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// The provider.
    pub provider: Provider,
    /// In words: not compiled into this build, no such device, the driver failed.
    pub reason: String,
}

fn runtime_error(error: &ort::Error) -> LoadError {
    LoadError::Runtime(error.to_string())
}

/// The element types `omuzaki-ai-api` knows. Others (`f64`, `bool`, strings, 8 and 16-bit integers and `bfloat16`)
/// are refused with a clear message when the model is probed, rather than failing at run time.
fn dtype(port: &str, ty: TensorElementType) -> Result<DType, LoadError> {
    match ty {
        TensorElementType::Float32 => Ok(DType::F32),
        TensorElementType::Int64 => Ok(DType::I64),
        TensorElementType::Int32 => Ok(DType::I32),
        TensorElementType::Uint8 => Ok(DType::U8),
        TensorElementType::Float16 => Ok(DType::F16),
        other => Err(LoadError::Format(format!(
            "`{port}` has element type {other:?}, which this runtime does not carry yet"
        ))),
    }
}

fn spec(outlet: &Outlet) -> Result<TensorSpec, LoadError> {
    let name = outlet.name();
    match outlet.dtype() {
        ValueType::Tensor { ty, shape, .. } => {
            let dims = shape.iter().map(|&d| usize::try_from(d).map_or(Dim::ANY, Dim::Fixed)).collect::<Vec<_>>();
            Ok(TensorSpec::new(name, dtype(name, *ty)?, dims))
        }
        _ => {
            Err(LoadError::Format(format!("`{name}` is not a tensor (sequences, maps and optionals are not carried)")))
        }
    }
}

/// The model's own inputs and outputs, dynamic dimensions as `any`.
pub fn signature_of(session: &Session) -> Result<Signature, LoadError> {
    Ok(Signature {
        inputs: session.inputs().iter().map(spec).collect::<Result<_, _>>()?,
        outputs: session.outputs().iter().map(spec).collect::<Result<_, _>>()?,
    })
}

/// A builder with the settings every session of ours has. `threads` is ONNX Runtime's intra-op pool; with one thread
/// the work runs on the caller's. Inter-op is 1 and neither pool spins while idle: `cpu.total` counts every cycle
/// (spec §10), and a spinning pool burns a core between runs. The config keys are the documented
/// `session.{intra,inter}_op.allow_spinning`.
fn builder(threads: u16) -> Result<SessionBuilder, LoadError> {
    let build = || -> ort::Result<SessionBuilder> {
        Ok(Session::builder()?
            .with_intra_threads(usize::from(threads.max(1)))?
            .with_inter_threads(1)?
            .with_intra_op_spinning(false)?
            .with_inter_op_spinning(false)?)
    };
    build().map_err(|e| runtime_error(&e))
}

/// The registration `ort` makes for `provider`, or why we can't: a provider this crate doesn't know.
fn dispatch(provider: Provider) -> Option<(ExecutionProviderDispatch, bool)> {
    use ort::ep;
    Some(match provider {
        Provider::Cuda => {
            let ep = ep::CUDA::default();
            (ep.clone().build().error_on_failure(), ep.is_available().unwrap_or(false))
        }
        Provider::DirectMl => {
            let ep = ep::DirectML::default();
            (ep.clone().build().error_on_failure(), ep.is_available().unwrap_or(false))
        }
        Provider::CoreMl => {
            let ep = ep::CoreML::default();
            (ep.clone().build().error_on_failure(), ep.is_available().unwrap_or(false))
        }
        Provider::WebGpu => {
            let ep = ep::WebGPU::default();
            (ep.clone().build().error_on_failure(), ep.is_available().unwrap_or(false))
        }
        _ => return None,
    })
}

/// Commits `model` as a session on `provider` (the CPU needs no registration). `Err` carries the reason in words.
fn commit(model: &[u8], options: &SessionOptions, provider: Provider, build: &str) -> Result<Session, String> {
    let mut builder = builder(options.threads).map_err(|e| e.to_string())?;
    if provider != Provider::Cpu {
        let Some((dispatch, available)) = dispatch(provider) else {
            return Err("this runtime does not know the provider".to_owned());
        };
        if !available {
            return Err(format!(
                "the ONNX Runtime build `{build}` this process loaded was not compiled with it (the build is fixed by the first load; \
                 the app sets it with `prefer_providers` before the first probe)"
            ));
        }
        builder = builder.with_execution_providers([dispatch]).map_err(|e| e.to_string())?;
    }
    builder.commit_from_memory(model).map_err(|e| e.to_string())
}

/// Loads `model` on the first provider of `options.providers` that works, then on the CPU. A provider that the
/// build lacks, or whose device or driver fails, is skipped and recorded; the CPU is always the last resort.
///
/// # Errors
/// [`LoadError::Runtime`] when even the CPU can't load the model (the bytes aren't a model ONNX Runtime accepts).
pub fn create(
    model: &[u8],
    options: &SessionOptions,
    build: &str,
) -> Result<(Session, Provider, Vec<Skipped>), LoadError> {
    let mut skipped = Vec::new();
    let mut tried = Vec::new();
    for &provider in options.providers.iter().filter(|p| **p != Provider::Cpu) {
        if tried.contains(&provider) {
            continue;
        }
        tried.push(provider);
        match commit(model, options, provider, build) {
            Ok(session) => return Ok((session, provider, skipped)),
            Err(reason) => {
                tracing::warn!(?provider, %reason, "provider skipped; trying the next, ending on the CPU");
                skipped.push(Skipped { provider, reason });
            }
        }
    }
    let session = commit(model, options, Provider::Cpu, build).map_err(LoadError::Runtime)?;
    Ok((session, Provider::Cpu, skipped))
}
