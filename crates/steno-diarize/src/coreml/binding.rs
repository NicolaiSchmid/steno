//! The `CoreML` calls behind [`super::CoreMlBackend`]: load a compiled
//! model, read an input's declared shape, build `MLMultiArray` inputs,
//! predict, read outputs. Every `unsafe` block in the crate is here, each
//! calling an Objective-C method through `objc2` with arguments that the
//! surrounding Rust has already checked; the invariants are noted at each
//! block.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_ml::{
    MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel, MLModelConfiguration,
    MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

use crate::backend::BackendError;

/// A compiled model.
pub struct Model {
    inner: Retained<MLModel>,
}

// SAFETY: `MLModel` is documented thread-safe (predictions may run from
// any thread, and from several at once); the retained pointer is the only
// field, and the pipeline moves the backend between threads but never
// shares it, so `Send` is all that is claimed.
unsafe impl Send for Model {}

impl Model {
    /// Loads the `.mlmodelc` directory at `path` on the default compute
    /// units (CPU, GPU and Neural Engine as `CoreML` sees fit).
    pub fn load(path: &std::path::Path) -> Result<Self, BackendError> {
        let text = path.to_string_lossy();
        // SAFETY: `NSURL` and `MLModelConfiguration` are plain Foundation
        // objects built from owned values; `modelWithContentsOfURL` reads
        // the directory and returns either a model or an `NSError`, both
        // retained.
        unsafe {
            let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&text), true);
            let configuration = MLModelConfiguration::new();
            let inner =
                MLModel::modelWithContentsOfURL_configuration_error(&url, &configuration)
                    .map_err(|error| format!("loading {text}: {}", error.localizedDescription()))?;
            Ok(Model { inner })
        }
    }

    /// The declared shape of input `name`, when the model declares one.
    pub fn input_shape(&self, name: &str) -> Option<Vec<usize>> {
        // SAFETY: the description, its dictionary and the constraint are
        // retained Objective-C objects owned by the model; reading them
        // has no preconditions.
        unsafe {
            let description = self.inner.modelDescription();
            let inputs = description.inputDescriptionsByName();
            let feature = inputs.objectForKey(&NSString::from_str(name))?;
            let constraint = feature.multiArrayConstraint()?;
            Some(read_numbers(&constraint.shape()))
        }
    }

    /// Runs the model on `inputs`.
    pub fn predict(&self, inputs: &[(&str, &Array)]) -> Result<Features, BackendError> {
        let keys: Vec<Retained<NSString>> = inputs
            .iter()
            .map(|(key, _)| NSString::from_str(key))
            .collect();
        // SAFETY: `featureValueWithMultiArray` retains the array; the
        // dictionary holds the retained values for as long as the provider
        // lives, which is the duration of the prediction.
        let values: Vec<Retained<MLFeatureValue>> = inputs
            .iter()
            .map(|(_, array)| unsafe { MLFeatureValue::featureValueWithMultiArray(&array.inner) })
            .collect();
        let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
        let value_refs: Vec<&AnyObject> =
            values.iter().map(|value| -> &AnyObject { value }).collect();
        let dictionary: Retained<NSDictionary<NSString, AnyObject>> =
            NSDictionary::from_slices(&key_refs, &value_refs);
        // SAFETY: the provider is built from a dictionary of feature values,
        // the documented input of `initWithDictionary:error:`; the
        // prediction takes a protocol object to that provider and returns a
        // retained provider or an `NSError`.
        unsafe {
            let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
                MLDictionaryFeatureProvider::alloc(),
                &dictionary,
            )
            .map_err(|error| format!("feature provider: {}", error.localizedDescription()))?;
            let object: &ProtocolObject<dyn MLFeatureProvider> =
                ProtocolObject::from_ref(&*provider);
            let output = self
                .inner
                .predictionFromFeatures_error(object)
                .map_err(|error| format!("prediction: {}", error.localizedDescription()))?;
            Ok(Features { inner: output })
        }
    }
}

/// The outputs of one prediction.
pub struct Features {
    inner: Retained<ProtocolObject<dyn MLFeatureProvider>>,
}

impl Features {
    /// Output `name` as an array.
    pub fn array(&self, name: &str) -> Result<Array, BackendError> {
        // SAFETY: `featureValueForName` and `multiArrayValue` read retained
        // objects from the provider; both return `None` rather than fail.
        unsafe {
            let value = self
                .inner
                .featureValueForName(&NSString::from_str(name))
                .ok_or_else(|| format!("output {name} missing"))?;
            let array = value
                .multiArrayValue()
                .ok_or_else(|| format!("output {name} is not a multi-array"))?;
            Ok(Array::wrap(array))
        }
    }

    /// The first multi-array output, whatever its name.
    pub fn first_array(&self) -> Result<Array, BackendError> {
        // SAFETY: `featureNames` is a retained set of strings owned by the
        // provider; iterating it has no preconditions.
        let names: Vec<String> = unsafe {
            let names = self.inner.featureNames();
            names.iter().map(|name| name.to_string()).collect()
        };
        let mut names = names;
        names.sort();
        for name in names {
            if let Ok(array) = self.array(&name) {
                return Ok(array);
            }
        }
        Err("the model returned no multi-array output".into())
    }
}

/// An `MLMultiArray` of `Float32` with its shape and strides in elements.
pub struct Array {
    inner: Retained<MLMultiArray>,
    shape: Vec<usize>,
    strides: Vec<usize>,
}

impl Array {
    /// A `Float32` array of `shape` filled with `values`, which must have
    /// exactly as many elements as the shape.
    pub fn from_f32(shape: &[usize], values: &[f32]) -> Result<Self, BackendError> {
        let count: usize = shape.iter().product();
        if count != values.len() {
            return Err(format!(
                "array of shape {shape:?} holds {count} values, given {}",
                values.len()
            )
            .into());
        }
        // SAFETY: a freshly allocated contiguous `Float32` array of `count`
        // elements; `dataPointer` points at `count * 4` writable bytes for
        // the array's lifetime and nothing else aliases it yet. The method
        // is deprecated in favour of the block-based accessors, which
        // objc2 exposes only through `block2`; the pointer form is what the
        // spike validated.
        #[allow(deprecated)]
        unsafe {
            let inner = MLMultiArray::initWithShape_dataType_error(
                MLMultiArray::alloc(),
                &ns_numbers(shape),
                MLMultiArrayDataType::Float32,
            )
            .map_err(|error| format!("MLMultiArray: {}", error.localizedDescription()))?;
            let pointer: NonNull<c_void> = inner.dataPointer();
            std::ptr::copy_nonoverlapping(values.as_ptr(), pointer.as_ptr().cast::<f32>(), count);
            Ok(Array::wrap(inner))
        }
    }

    fn wrap(inner: Retained<MLMultiArray>) -> Self {
        // SAFETY: `shape` and `strides` are retained arrays of numbers the
        // multi-array owns.
        let (shape, strides) =
            unsafe { (read_numbers(&inner.shape()), read_numbers(&inner.strides())) };
        Array {
            inner,
            shape,
            strides,
        }
    }

    pub fn shape(&self) -> &[usize] {
        &self.shape
    }

    fn count(&self) -> usize {
        self.shape.iter().product()
    }

    fn is_contiguous(&self) -> bool {
        let mut expected = 1;
        for (size, stride) in self.shape.iter().rev().zip(self.strides.iter().rev()) {
            if *stride != expected {
                return false;
            }
            expected *= size;
        }
        true
    }

    /// The elements in row-major order.
    pub fn to_f32(&self) -> Result<Vec<f32>, BackendError> {
        // SAFETY: `dataType` and `dataPointer` read the array's own
        // metadata and buffer. The buffer is read only inside this call,
        // for `count` elements, and only when the array is `Float32` and
        // contiguous (checked first), so the slice is in bounds and typed
        // correctly; a strided array is gathered element by element with
        // offsets computed from its own strides. `dataPointer` as above.
        #[allow(deprecated)]
        unsafe {
            if self.inner.dataType() != MLMultiArrayDataType::Float32 {
                return Err("`CoreML` output is not Float32".into());
            }
            let base = self.inner.dataPointer().as_ptr().cast::<f32>();
            if self.is_contiguous() {
                return Ok(std::slice::from_raw_parts(base, self.count()).to_vec());
            }
            let mut values = Vec::with_capacity(self.count());
            let mut index = vec![0usize; self.shape.len()];
            for _ in 0..self.count() {
                let offset: usize = index.iter().zip(&self.strides).map(|(i, s)| i * s).sum();
                values.push(*base.add(offset));
                for axis in (0..self.shape.len()).rev() {
                    index[axis] += 1;
                    if index[axis] < self.shape[axis] {
                        break;
                    }
                    index[axis] = 0;
                }
            }
            Ok(values)
        }
    }
}

fn ns_numbers(values: &[usize]) -> Retained<NSArray<NSNumber>> {
    let numbers: Vec<Retained<NSNumber>> = values
        .iter()
        .map(|value| NSNumber::numberWithInteger(isize::try_from(*value).unwrap_or(isize::MAX)))
        .collect();
    NSArray::from_retained_slice(&numbers)
}

fn read_numbers(array: &NSArray<NSNumber>) -> Vec<usize> {
    array
        .iter()
        .map(|number| usize::try_from(number.integerValue()).unwrap_or(0))
        .collect()
}
