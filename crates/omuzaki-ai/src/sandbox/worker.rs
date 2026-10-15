//! The worker's side: [`serve`] reads requests from stdin and answers on stdout until the registry closes the pipe.
//!
//! It holds at most one session. A runtime that panics, or a session that reports a crash, ends the process: the
//! registry sees the pipe close and treats the model as crashed, which is the point of running it here.

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Mutex, PoisonError};

use omuzaki_ai_api::{
    FetchError, FileSource, LoadError, LocalFile, ModelSource, PinnedFile, Progress, RunError, Session, TensorBuf,
    TensorRef, find_runtime,
};

use super::limits;
use super::wire::{
    self, Control, Job, Kind, MAGIC, WireLoadError, WireRunError, broken, read_frame, write_control, write_frame,
};

/// The pipe to the registry. The loop and [`Parent`] (the runtime's native-library source) share it, never at once.
struct Pipe<R, W> {
    input: R,
    output: W,
    buf: Vec<u8>,
}

impl<R: Read, W: Write> Pipe<R, W> {
    fn send(&mut self, message: &Control) -> io::Result<()> {
        write_control(&mut self.output, message)?;
        self.output.flush()
    }
}

/// Serves the registry on this process's stdin and stdout, under the limits the registry asked for (the module
/// documentation of `sandbox` lists them per OS). Returns when the registry closes the pipe; a protocol error is
/// reported on stderr and ends the worker with a failure code.
pub fn serve() -> ExitCode {
    limits::enter_worker();
    let input = BufReader::new(io::stdin());
    let output = BufWriter::new(io::stdout());
    match serve_on(input, output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omuzaki model worker: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The loop, over any pipe.
fn serve_on<R: Read + Send, W: Write + Send>(input: R, output: W) -> io::Result<()> {
    let pipe = Mutex::new(Pipe { input, output, buf: Vec::new() });
    {
        let mut p = lock(&pipe);
        p.output.write_all(MAGIC)?;
        p.output.flush()?;
    }
    let mut session: Option<Box<dyn Session>> = None;
    let (mut inputs, mut outputs, mut scratch): (Vec<TensorBuf>, Vec<TensorBuf>, Vec<u8>) = Default::default();
    loop {
        let kind = {
            let mut p = lock(&pipe);
            let Pipe { input, buf, .. } = &mut *p;
            match read_frame(input, buf)? {
                Some(kind) => kind,
                None => return Ok(()),
            }
        };
        match kind {
            Kind::Control => {
                let message = wire::control(&lock(&pipe).buf)?;
                let answer = handle(message, &pipe, &mut session, &mut outputs);
                lock(&pipe).send(&answer)?;
            }
            Kind::Run => {
                let n = wire::read_count(&lock(&pipe).buf)?;
                let mut p = lock(&pipe);
                read_inputs(&mut *p, n, &mut inputs)?;
                let Some(running) = session.as_mut() else { return Err(broken("a run before a load")) };
                let refs: Vec<TensorRef<'_>> = inputs.iter().map(TensorBuf::as_ref).collect();
                match running.run(&refs, &mut outputs) {
                    Ok(()) => {
                        write_frame(&mut p.output, Kind::Ran, &wire::count(outputs.len()))?;
                        for out in &outputs {
                            wire::encode_tensor(&out.as_ref(), &mut scratch)?;
                            write_frame(&mut p.output, Kind::Tensor, &scratch)?;
                        }
                        p.output.flush()?;
                    }
                    Err(error) => {
                        p.send(&Control::RunFailed { error: WireRunError::from(&error) })?;
                        if error == RunError::Crashed {
                            // The session says it is dead: so is this worker, and the registry falls back.
                            return Ok(());
                        }
                    }
                }
            }
            Kind::Reset => {
                if let Some(running) = session.as_mut() {
                    running.reset();
                }
            }
            Kind::Tensor | Kind::Ran => return Err(broken(format!("a {kind:?} frame out of place"))),
        }
    }
}

/// Reads `n` tensor frames into `inputs`, reusing their storage.
fn read_inputs<R: Read, W: Write>(p: &mut Pipe<R, W>, n: usize, inputs: &mut Vec<TensorBuf>) -> io::Result<()> {
    inputs.truncate(n);
    for i in 0..n {
        if read_frame(&mut p.input, &mut p.buf)? != Some(Kind::Tensor) {
            return Err(broken("a run without its input tensors"));
        }
        let dtype = wire::frame_dtype(&p.buf)?;
        if inputs.get(i).is_none_or(|t| t.dtype() != dtype) {
            inputs.truncate(i);
            inputs.push(TensorBuf::new(dtype));
        }
        wire::decode_tensor(&p.buf, &mut inputs[i])?;
    }
    Ok(())
}

/// Answers a probe or a load.
fn handle<R: Read + Send, W: Write + Send>(
    message: Control,
    pipe: &Mutex<Pipe<R, W>>,
    session: &mut Option<Box<dyn Session>>,
    outputs: &mut Vec<TensorBuf>,
) -> Control {
    let natives = Parent { pipe };
    let result = match message {
        Control::Probe { job } => with_source(&job, &natives, |runtime, source| runtime.probe(source))
            .map(|probe| Control::Probed { signature: probe.signature, opset: probe.opset }),
        Control::Load { job, slot, io, options } => {
            // One session at a time: a new load replaces the old one, which goes first to free its memory.
            *session = None;
            with_source(&job, &natives, |runtime, source| runtime.load(source, &slot, &io, &options)).map(|loaded| {
                *outputs = TensorBuf::for_outputs(loaded.signature());
                let provider = loaded.info().provider;
                *session = Some(loaded);
                Control::Loaded { provider }
            })
        }
        other => Err(LoadError::Runtime(format!("the worker can't answer {other:?}"))),
    };
    result.unwrap_or_else(|e| Control::Failed { error: WireLoadError::from(&e) })
}

/// Runs `f` with the job's runtime and its files as a [`ModelSource`].
fn with_source<T>(
    job: &Job,
    natives: &dyn FileSource,
    f: impl FnOnce(&dyn omuzaki_ai_api::Runtime, &ModelSource<'_>) -> Result<T, LoadError>,
) -> Result<T, LoadError> {
    let runtime = find_runtime(&job.runtime)
        .ok_or_else(|| LoadError::Runtime(format!("this app has no `{}` runtime to read it with", job.runtime)))?;
    let files: Vec<LocalFile> =
        job.files.iter().map(|(name, path)| LocalFile { name: name.clone(), path: path.clone() }).collect();
    let source = ModelSource { model: &job.model, files: &files, stages: &job.stages, natives };
    f(&*runtime, &source)
}

/// The runtime's native libraries, fetched by the registry: the worker asks over the pipe and waits for the path.
struct Parent<'a, R, W> {
    pipe: &'a Mutex<Pipe<R, W>>,
}

impl<R: Read + Send, W: Write + Send> FileSource for Parent<'_, R, W> {
    fn get(&self, pin: &PinnedFile, _progress: &dyn Progress) -> Result<PathBuf, FetchError> {
        let io_error = |e: io::Error| FetchError::Io { path: "the registry's pipe".into(), reason: e.to_string() };
        let mut p = lock(self.pipe);
        p.send(&Control::Fetch { urls: pin.urls.clone(), sha256: pin.sha256, size: pin.size }).map_err(io_error)?;
        let Pipe { input, buf, .. } = &mut *p;
        if read_frame(input, buf).map_err(io_error)? != Some(Kind::Control) {
            return Err(io_error(broken("no answer to a fetch")));
        }
        match wire::control(buf).map_err(io_error)? {
            Control::Fetched { result: Ok(path) } => Ok(path),
            Control::Fetched { result: Err(reason) } => {
                Err(FetchError::Io { path: format!("the library {}", pin.sha256), reason })
            }
            other => Err(io_error(broken(format!("{other:?} where a fetch answer was expected")))),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
