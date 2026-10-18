//! The safe wrapper over `objc2-core-ml`: load a compiled `.mlmodelc`,
//! allocate `MLMultiArray` inputs, run a prediction, read the outputs, and
//! [`contained`], the pool and `@catch` every call runs in. The only module
//! in the crate with `unsafe`; every block states the invariant it relies on.

// `dataPointer` is a `void *`; CoreML allocates `MLMultiArray` storage
// aligned for its element type (the Swift side binds it with
// `bindMemory(to: Float.self)` the same way), so the `u8` to `f32`/`i32`
// casts below are aligned. Stated once here, checked by `require` before
// every slice.
#![allow(clippy::cast_ptr_alignment)]

use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use objc2::AnyThread;
use objc2::exception::Exception;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel,
    MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSException, NSNumber, NSString, NSURL};

use crate::SpeechError;

/// Runs `work` (one model call or load) inside one autorelease pool and an
/// Objective-C `@catch` ([`objc2::exception::catch`]). Every CoreML call goes
/// through here ([`crate::backend`]).
///
/// - **The pool**: outputs come back at +0 and a missed fast autorelease
///   handshake leaves them in the thread's pool; without a pool per call,
///   long transcriptions leaked IOSurfaces until allocation failed.
/// - **The catch**: an exception reaching a Rust frame aborts the process.
///   Caught, it becomes [`SpeechError::Exception`], so the meeting fails like
///   any speech failure and its audio is kept. A C++ exception or an abort
///   inside CoreML still ends the process.
///
/// The catch needs Rust frames that unwind, so a `panic = "abort"` build
/// does not compile.
pub fn contained<T>(work: impl FnOnce() -> Result<T, SpeechError>) -> Result<T, SpeechError> {
    // Unwind safety, asserted: `work` only reads the shared models and builds
    // fresh arrays, and the engine drops its models after an exception
    // (`CoreMlParakeetEngine`), so nothing half updated is used again.
    objc2::exception::catch(AssertUnwindSafe(|| autoreleasepool(|_| work())))
        .unwrap_or_else(|exception| Err(exception_error(exception)))
}

#[cfg(panic = "abort")]
compile_error!(
    "steno-speech-coreml needs panic = \"unwind\": `coreml::contained` catches CoreML's \
     Objective-C exceptions by unwinding the Rust frames between the throw and the catch"
);

/// The error for a caught exception: an `NSException`'s name and reason;
/// any other object thrown (the runtime allows it) by its class.
fn exception_error(exception: Option<Retained<Exception>>) -> SpeechError {
    let (name, reason) = match exception.map(NSException::from_exception) {
        Some(Ok(exception)) => (
            exception.name().to_string(),
            exception
                .reason()
                .map(|reason| reason.to_string())
                .unwrap_or_default(),
        ),
        Some(Err(other)) => (
            other.class().name().to_string_lossy().into_owned(),
            String::new(),
        ),
        None => ("nil".to_owned(), String::new()),
    };
    SpeechError::Exception { name, reason }
}

/// Where a model runs. The names are CoreML's; the Parakeet defaults are in
/// [`crate::backend::ComputeUnitsPlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeUnits {
    CpuOnly,
    CpuAndGpu,
    CpuAndNeuralEngine,
    All,
}

impl ComputeUnits {
    fn raw(self) -> MLComputeUnits {
        match self {
            ComputeUnits::CpuOnly => MLComputeUnits::CPUOnly,
            ComputeUnits::CpuAndGpu => MLComputeUnits::CPUAndGPU,
            ComputeUnits::CpuAndNeuralEngine => MLComputeUnits::CPUAndNeuralEngine,
            ComputeUnits::All => MLComputeUnits::All,
        }
    }
}

impl fmt::Display for ComputeUnits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ComputeUnits::CpuOnly => "cpuOnly",
            ComputeUnits::CpuAndGpu => "cpuAndGPU",
            ComputeUnits::CpuAndNeuralEngine => "cpuAndNeuralEngine",
            ComputeUnits::All => "all",
        })
    }
}

/// Element type of an [`Array`]; the pipeline uses two of CoreML's five.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    Float32,
    Int32,
    /// Anything else CoreML can hand back; never allocated here.
    Other,
}

impl DataType {
    fn from_raw(raw: MLMultiArrayDataType) -> DataType {
        match raw {
            MLMultiArrayDataType::Float32 => DataType::Float32,
            MLMultiArrayDataType::Int32 => DataType::Int32,
            _ => DataType::Other,
        }
    }
}

fn ns_error(error: &objc2_foundation::NSError) -> String {
    error.localizedDescription().to_string()
}

/// A loaded `MLModel`.
///
/// `Send + Sync` is asserted below: Apple documents `MLModel` prediction as
/// safe to call from several threads at once, and FluidAudio relies on it
/// the same way, running four chunk workers over one `AsrModels` instance
/// (`AsrManager.makeWorkerClone` shares the models, not copies). The
/// configuration object is consumed at load and never touched again.
pub struct Model {
    inner: Retained<MLModel>,
    path: PathBuf,
    units: ComputeUnits,
}

// SAFETY: `MLModel` is documented thread-safe for `prediction(from:)`
// (Core ML, "Making Predictions with a Model": the same model instance may
// serve concurrent predictions). `Model` exposes nothing but `predict`, so
// sharing a `&Model` between threads shares only that call. `Retained`
// itself is reference counted with atomic operations.
unsafe impl Send for Model {}
// SAFETY: as above; `predict` takes `&self` and performs no interior
// mutation visible to Rust.
unsafe impl Sync for Model {}

impl fmt::Debug for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Model")
            .field("path", &self.path)
            .field("units", &self.units)
            .finish_non_exhaustive()
    }
}

impl Model {
    /// Load the compiled bundle at `path` (a `.mlmodelc` directory) with
    /// `units`; a cold load takes seconds. Run it inside [`contained`].
    pub(crate) fn load(path: &Path, units: ComputeUnits) -> Result<Model, SpeechError> {
        let load_error = |message: String| SpeechError::ModelLoad {
            path: path.to_path_buf(),
            message,
        };
        if !path.is_dir() {
            return Err(load_error("not a directory".to_owned()));
        }
        let url =
            NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&path.to_string_lossy()), true);
        // SAFETY: `MLModelConfiguration::new` and the two setters are plain
        // Objective-C property calls on a fresh object we own;
        // `modelWithContentsOfURL_configuration_error` reads the bundle
        // and either returns a retained model or an error, both owned.
        // `allowLowPrecisionAccumulationOnGPU` is what FluidAudio's
        // `MLModelConfigurationUtils.defaultConfiguration` sets.
        let inner = unsafe {
            let configuration = MLModelConfiguration::new();
            configuration.setComputeUnits(units.raw());
            configuration.setAllowLowPrecisionAccumulationOnGPU(true);
            MLModel::modelWithContentsOfURL_configuration_error(&url, &configuration)
                .map_err(|error| load_error(ns_error(&error)))?
        };
        Ok(Model {
            inner,
            path: path.to_path_buf(),
            units,
        })
    }

    /// The compute units the model was loaded with.
    #[must_use]
    pub fn units(&self) -> ComputeUnits {
        self.units
    }

    /// One prediction. Run it inside [`contained`] together with building its
    /// inputs and reading its outputs.
    pub(crate) fn predict(&self, inputs: &Inputs) -> Result<Outputs, SpeechError> {
        let provider: &ProtocolObject<dyn MLFeatureProvider> =
            ProtocolObject::from_ref(&*inputs.inner);
        // SAFETY: `predictionFromFeatures_error` takes a feature provider
        // whose arrays we allocated and filled completely (`Array`
        // zero-fills at allocation), and returns a retained provider or a
        // retained error. The model is not mutated.
        let outputs = unsafe { self.inner.predictionFromFeatures_error(provider) }
            .map_err(|error| SpeechError::CoreMl(ns_error(&error)))?;
        Ok(Outputs { inner: outputs })
    }
}

/// An `MLMultiArray` with its shape and strides read once, in elements.
///
/// Arrays allocated here are first-major and contiguous, so the buffer
/// holds exactly `len()` elements of `data_type`; arrays returned by a
/// model may be strided (the encoder output is), and the typed accessors
/// refuse those, leaving [`EncoderView`] to read them by index.
pub struct Array {
    inner: Retained<MLMultiArray>,
    shape: Vec<usize>,
    strides: Vec<usize>,
    data_type: DataType,
}

impl fmt::Debug for Array {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Array")
            .field("shape", &self.shape)
            .field("strides", &self.strides)
            .field("data_type", &self.data_type)
            .finish_non_exhaustive()
    }
}

fn ns_numbers(values: &[usize]) -> Retained<NSArray<NSNumber>> {
    let numbers: Vec<Retained<NSNumber>> = values
        .iter()
        .map(|&value| {
            // Shapes are small; `usize -> isize` cannot overflow here.
            NSNumber::numberWithInteger(isize::try_from(value).expect("shape fits isize"))
        })
        .collect();
    NSArray::from_retained_slice(&numbers)
}

/// Shape or stride entries as `usize`. CoreML reports them as `NSInteger`
/// and permits negative strides; one would alias frame 0 in
/// `EncoderView::copy_frame`, so a negative entry is an error instead.
fn read_numbers(array: &NSArray<NSNumber>, name: &str) -> Result<Vec<usize>, SpeechError> {
    (0..array.count())
        .map(|index| {
            let value = array.objectAtIndex(index).integerValue();
            usize::try_from(value).map_err(|_| {
                SpeechError::CoreMl(format!("{name} entry {index} is negative: {value}"))
            })
        })
        .collect()
}

impl Array {
    fn alloc(shape: &[usize], data_type: MLMultiArrayDataType) -> Result<Array, SpeechError> {
        // SAFETY: `initWithShape_dataType_error` allocates a first-major
        // contiguous buffer of `product(shape)` elements, uninitialised;
        // `wrap` reads the layout CoreML chose.
        let inner = unsafe {
            MLMultiArray::initWithShape_dataType_error(
                MLMultiArray::alloc(),
                &ns_numbers(shape),
                data_type,
            )
        }
        .map_err(|error| SpeechError::CoreMl(ns_error(&error)))?;
        let array = Array::wrap(inner)?;
        let element_size = match array.data_type {
            DataType::Float32 => size_of::<f32>(),
            DataType::Int32 => size_of::<i32>(),
            DataType::Other => {
                return Err(SpeechError::CoreMl(
                    "allocated an unsupported data type".to_owned(),
                ));
            }
        };
        array.require_contiguous()?;
        // SAFETY: contiguous (per `require_contiguous`), so the buffer is
        // `len() * size_of::<T>()` bytes owned by the retained array, and
        // `write_bytes` initialises every byte before a slice is formed.
        unsafe { std::ptr::write_bytes(array.base(), 0, array.len() * element_size) };
        Ok(array)
    }

    /// A zero-filled `f32` array.
    pub(crate) fn zeros_f32(shape: &[usize]) -> Result<Array, SpeechError> {
        Array::alloc(shape, MLMultiArrayDataType::Float32)
    }

    /// A zero-filled `i32` array.
    pub(crate) fn zeros_i32(shape: &[usize]) -> Result<Array, SpeechError> {
        Array::alloc(shape, MLMultiArrayDataType::Int32)
    }

    fn wrap(inner: Retained<MLMultiArray>) -> Result<Array, SpeechError> {
        // SAFETY: `shape`, `strides` and `dataType` are read-only
        // properties of a live array.
        let (shape, strides, data_type) = unsafe {
            (
                read_numbers(&inner.shape(), "shape")?,
                read_numbers(&inner.strides(), "strides")?,
                DataType::from_raw(inner.dataType()),
            )
        };
        Ok(Array {
            inner,
            shape,
            strides,
            data_type,
        })
    }

    #[must_use]
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }

    #[must_use]
    pub fn strides(&self) -> &[usize] {
        &self.strides
    }

    #[must_use]
    pub fn data_type(&self) -> DataType {
        self.data_type
    }

    /// Elements addressed by the shape.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shape.iter().product()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the strides are the first-major dense ones, so the buffer
    /// is exactly `len()` consecutive elements.
    #[must_use]
    pub fn is_contiguous(&self) -> bool {
        let mut expected = 1usize;
        for (size, stride) in self.shape.iter().rev().zip(self.strides.iter().rev()) {
            if *stride != expected {
                return false;
            }
            expected *= size;
        }
        true
    }

    fn require_type(&self, data_type: DataType) -> Result<(), SpeechError> {
        if self.data_type != data_type {
            return Err(SpeechError::CoreMl(format!(
                "array is {:?}, wanted {data_type:?}",
                self.data_type
            )));
        }
        Ok(())
    }

    fn require_contiguous(&self) -> Result<(), SpeechError> {
        if !self.is_contiguous() {
            return Err(SpeechError::CoreMl(format!(
                "array {:?} with strides {:?} is not contiguous",
                self.shape, self.strides
            )));
        }
        Ok(())
    }

    /// `require_type` plus contiguity, the precondition of the typed slices.
    fn require(&self, data_type: DataType) -> Result<(), SpeechError> {
        self.require_type(data_type)?;
        self.require_contiguous()
    }

    /// The base pointer. `dataPointer` is deprecated in favour of
    /// `getBytesWithHandler`, which needs `block2` closures; the pointer stays
    /// valid for the array's lifetime, which every caller here respects by
    /// holding `&self` for as long as the slice lives.
    #[allow(deprecated)]
    fn base(&self) -> *mut u8 {
        // SAFETY: `dataPointer` is a property read on a live array.
        unsafe { self.inner.dataPointer().as_ptr().cast::<u8>() }
    }

    /// The elements as `f32`, for a contiguous `f32` array.
    pub fn as_f32(&self) -> Result<&[f32], SpeechError> {
        self.require(DataType::Float32)?;
        // SAFETY: contiguous `Float32` array, so the buffer holds `len()`
        // aligned `f32`s (CoreML allocates element-aligned storage); the
        // slice borrows `self`, which keeps the array alive, and `&self`
        // forbids a concurrent `as_f32_mut`.
        Ok(unsafe { std::slice::from_raw_parts(self.base().cast::<f32>(), self.len()) })
    }

    /// The elements as mutable `f32`, for a contiguous `f32` array.
    pub fn as_f32_mut(&mut self) -> Result<&mut [f32], SpeechError> {
        self.require(DataType::Float32)?;
        // SAFETY: as `as_f32`; `&mut self` makes this the only live view.
        Ok(unsafe { std::slice::from_raw_parts_mut(self.base().cast::<f32>(), self.len()) })
    }

    /// The elements as `i32`, for a contiguous `i32` array.
    pub fn as_i32(&self) -> Result<&[i32], SpeechError> {
        self.require(DataType::Int32)?;
        // SAFETY: as `as_f32`, for `Int32`.
        Ok(unsafe { std::slice::from_raw_parts(self.base().cast::<i32>(), self.len()) })
    }

    /// The elements as mutable `i32`, for a contiguous `i32` array.
    pub fn as_i32_mut(&mut self) -> Result<&mut [i32], SpeechError> {
        self.require(DataType::Int32)?;
        // SAFETY: as `as_f32_mut`, for `Int32`.
        Ok(unsafe { std::slice::from_raw_parts_mut(self.base().cast::<i32>(), self.len()) })
    }

    /// The first element of an `i32` array (the length outputs).
    pub fn i32_scalar(&self) -> Result<i32, SpeechError> {
        self.as_i32()?
            .first()
            .copied()
            .ok_or_else(|| SpeechError::CoreMl("empty i32 array".to_owned()))
    }

    /// The first element of an `f32` array (the probability output).
    pub fn f32_scalar(&self) -> Result<f32, SpeechError> {
        self.as_f32()?
            .first()
            .copied()
            .ok_or_else(|| SpeechError::CoreMl("empty f32 array".to_owned()))
    }

    /// The elements of an `f32` array in first-major order, read through
    /// the strides, so a strided model output reads as a dense one.
    pub fn to_vec_f32(&self) -> Result<Vec<f32>, SpeechError> {
        self.require_type(DataType::Float32)?;
        let base = self.base().cast::<f32>();
        let mut out = Vec::with_capacity(self.len());
        let mut index = vec![0usize; self.shape.len()];
        for _ in 0..self.len() {
            let offset: usize = index
                .iter()
                .zip(&self.strides)
                .map(|(i, stride)| i * stride)
                .sum();
            // SAFETY: every `index[d] < shape[d]`, so `offset` addresses an
            // element of the buffer under CoreML's own (non-negative, per
            // `read_numbers`) strides; the array is alive for `&self`.
            out.push(unsafe { base.add(offset).read() });
            // The next index, last axis fastest.
            for d in (0..index.len()).rev() {
                index[d] += 1;
                if index[d] < self.shape[d] {
                    break;
                }
                index[d] = 0;
            }
        }
        Ok(out)
    }
}

/// The inputs of one prediction: a dictionary feature provider over
/// arrays the caller still owns. The arrays must outlive the call, which
/// the borrow in `inputs` guarantees.
pub struct Inputs {
    inner: Retained<MLDictionaryFeatureProvider>,
}

/// Build the provider for `items`, each a feature name and its array.
pub(crate) fn inputs(items: &[(&str, &Array)]) -> Result<Inputs, SpeechError> {
    let keys: Vec<Retained<NSString>> = items
        .iter()
        .map(|(name, _)| NSString::from_str(name))
        .collect();
    // SAFETY: `featureValueWithMultiArray` wraps a live array in a value
    // that retains it.
    let values: Vec<Retained<MLFeatureValue>> = items
        .iter()
        .map(|(_, array)| unsafe { MLFeatureValue::featureValueWithMultiArray(&array.inner) })
        .collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    let value_refs: Vec<&AnyObject> = values.iter().map(|value| -> &AnyObject { value }).collect();
    let dictionary: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&key_refs, &value_refs);
    // SAFETY: `initWithDictionary_error` copies the dictionary's entries
    // into a provider it owns; the values are `MLFeatureValue`s, the one
    // type the initialiser accepts.
    let inner = unsafe {
        MLDictionaryFeatureProvider::initWithDictionary_error(
            MLDictionaryFeatureProvider::alloc(),
            &dictionary,
        )
    }
    .map_err(|error| SpeechError::CoreMl(ns_error(&error)))?;
    Ok(Inputs { inner })
}

/// The outputs of one prediction.
pub struct Outputs {
    inner: Retained<ProtocolObject<dyn MLFeatureProvider>>,
}

impl Outputs {
    /// The multi-array output called `name`.
    pub(crate) fn array(&self, name: &'static str) -> Result<Array, SpeechError> {
        // SAFETY: `featureValueForName` and `multiArrayValue` are lookups
        // on a live provider that return retained objects or nil.
        let array = unsafe {
            self.inner
                .featureValueForName(&NSString::from_str(name))
                .and_then(|value| value.multiArrayValue())
        }
        .ok_or(SpeechError::MissingOutput(name))?;
        Array::wrap(array)
    }
}

/// A read-only view over the encoder output `[1, hidden, time]`, which
/// CoreML returns strided; frames are read through the strides, never
/// as a slice (`EncoderFrameView` in FluidAudio).
pub struct EncoderView {
    array: Array,
    hidden: usize,
    frames: usize,
    hidden_stride: usize,
    time_stride: usize,
    /// Frames the encoder marked valid (`encoder_length`).
    pub valid: usize,
}

impl fmt::Debug for EncoderView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncoderView")
            .field("hidden", &self.hidden)
            .field("frames", &self.frames)
            .field("valid", &self.valid)
            .finish_non_exhaustive()
    }
}

impl EncoderView {
    /// Wrap the encoder output; `valid` is clamped to the time axis.
    pub fn new(array: Array, hidden: usize, valid: usize) -> Result<EncoderView, SpeechError> {
        if array.shape.len() != 3 || array.shape[0] != 1 || array.shape[1] != hidden {
            return Err(SpeechError::Shape {
                name: "encoder",
                shape: array.shape.clone(),
            });
        }
        array.require_type(DataType::Float32)?;
        // A zero stride would make every frame read the same element.
        if array.strides[1] == 0 || array.strides[2] == 0 {
            return Err(SpeechError::CoreMl(format!(
                "encoder strides {:?} are not addressable",
                array.strides
            )));
        }
        let frames = array.shape[2];
        Ok(EncoderView {
            hidden,
            frames,
            hidden_stride: array.strides[1],
            time_stride: array.strides[2],
            valid: valid.min(frames),
            array,
        })
    }

    /// Frames along the time axis, valid or not.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Copy frame `t` into `dst` (`hidden` elements).
    pub fn copy_frame(&self, t: usize, dst: &mut [f32]) -> Result<(), SpeechError> {
        if t >= self.frames || dst.len() != self.hidden {
            return Err(SpeechError::CoreMl(format!(
                "encoder frame {t} of {} into {} elements",
                self.frames,
                dst.len()
            )));
        }
        let base = self.array.base().cast::<f32>();
        for (h, slot) in dst.iter_mut().enumerate() {
            let offset = h * self.hidden_stride + t * self.time_stride;
            // SAFETY: `h < hidden` and `t < frames`, both within the
            // array's shape, so `offset` addresses an element of the
            // buffer under CoreML's own strides; the array is alive for
            // `&self`.
            *slot = unsafe { base.add(offset).read() };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn raise(name: &str, reason: &str) -> ! {
        NSException::new(
            &NSString::from_str(name),
            Some(&NSString::from_str(reason)),
            None,
        )
        .expect("an exception object")
        .raise()
    }

    /// Sets its flag when dropped, so a test sees the Rust frames inside
    /// `contained` unwound.
    struct Dropped<'a>(&'a Cell<bool>);

    impl Drop for Dropped<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    /// An `NSException` raised inside comes out as an error with its name and
    /// reason, and the Rust values on the way are dropped.
    #[test]
    fn an_exception_inside_becomes_an_error_with_its_name_and_reason() {
        let dropped = Cell::new(false);
        let error = contained(|| -> Result<(), SpeechError> {
            let _guard = Dropped(&dropped);
            raise("StenoTestException", "thrown on purpose")
        })
        .unwrap_err();
        assert!(
            matches!(
                &error,
                SpeechError::Exception { name, reason }
                    if name == "StenoTestException" && reason == "thrown on purpose"
            ),
            "{error:?}"
        );
        assert!(error.is_exception());
        assert!(dropped.get(), "the frames inside were unwound");
        assert_eq!(contained(|| Ok(7)).unwrap(), 7);
    }

    /// The runtime lets any object be thrown; one that is not an
    /// `NSException` is named by its class.
    #[test]
    fn a_thrown_object_that_is_not_an_exception_is_named_by_its_class() {
        let thrown = NSString::from_str("not an exception");
        // SAFETY: `Exception` is a transparent wrapper over any object, and
        // `throw` hands the retained object to the unwinder unchanged.
        let thrown = unsafe { Retained::cast_unchecked::<Exception>(thrown) };
        let error = contained(|| -> Result<(), SpeechError> { objc2::exception::throw(thrown) })
            .unwrap_err();
        assert!(
            matches!(&error, SpeechError::Exception { name, reason } if name.contains("String") && reason.is_empty()),
            "{error:?}"
        );
    }

    /// Without an exception, `contained` hands back what the work returned.
    #[test]
    fn a_result_passes_through() {
        let error = contained(|| -> Result<(), SpeechError> {
            Err(SpeechError::CoreMl("refused".to_owned()))
        })
        .unwrap_err();
        assert!(matches!(error, SpeechError::CoreMl(text) if text == "refused"));
    }

    /// What a call leaves autoreleased is released by the time `contained`
    /// returns.
    #[test]
    fn what_a_call_leaves_autoreleased_is_released_before_it_returns() {
        let weak = contained(|| {
            let object = objc2::runtime::NSObject::new();
            let weak = objc2::rc::Weak::from_retained(&object);
            let _ = Retained::autorelease_ptr(object);
            assert!(weak.load().is_some(), "alive in the pool");
            Ok(weak)
        })
        .unwrap();
        assert!(weak.load().is_none(), "the pool drained it");
    }
}
