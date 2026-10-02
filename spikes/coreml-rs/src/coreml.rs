//! Minimal safe-ish wrapper over objc2-core-ml: load a compiled .mlmodelc,
//! build MLMultiArray inputs, run a prediction, read outputs.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::AnyThread;
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel,
    MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

pub type Features = Retained<ProtocolObject<dyn MLFeatureProvider>>;

pub fn parse_units(s: &str) -> MLComputeUnits {
    match s {
        "cpu" => MLComputeUnits::CPUOnly,
        "cpu-gpu" => MLComputeUnits::CPUAndGPU,
        "cpu-ane" => MLComputeUnits::CPUAndNeuralEngine,
        _ => MLComputeUnits::All,
    }
}

pub struct Model {
    inner: Retained<MLModel>,
}

impl Model {
    pub fn load(path: &str, units: MLComputeUnits) -> Result<Self, String> {
        unsafe {
            let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true);
            let cfg = MLModelConfiguration::new();
            cfg.setComputeUnits(units);
            let inner = MLModel::modelWithContentsOfURL_configuration_error(&url, &cfg)
                .map_err(|e| format!("load {path}: {}", e.localizedDescription()))?;
            Ok(Self { inner })
        }
    }

    pub fn predict(&self, input: &MLDictionaryFeatureProvider) -> Result<Features, String> {
        unsafe {
            let p: &ProtocolObject<dyn MLFeatureProvider> = ProtocolObject::from_ref(input);
            self.inner
                .predictionFromFeatures_error(p)
                .map_err(|e| format!("prediction: {}", e.localizedDescription()))
        }
    }
}

/// An MLMultiArray plus its shape and strides (in elements).
pub struct Array {
    pub inner: Retained<MLMultiArray>,
    pub shape: Vec<usize>,
    pub strides: Vec<usize>,
}

fn ns_numbers(v: &[usize]) -> Retained<NSArray<NSNumber>> {
    let nums: Vec<Retained<NSNumber>> =
        v.iter().map(|&x| NSNumber::numberWithInteger(x as isize)).collect();
    NSArray::from_retained_slice(&nums)
}

fn read_numbers(a: &NSArray<NSNumber>) -> Vec<usize> {
    let n = a.count();
    (0..n).map(|i| a.objectAtIndex(i).integerValue() as usize).collect()
}

impl Array {
    fn alloc(shape: &[usize], dt: MLMultiArrayDataType) -> Result<Self, String> {
        unsafe {
            let inner = MLMultiArray::initWithShape_dataType_error(
                MLMultiArray::alloc(),
                &ns_numbers(shape),
                dt,
            )
            .map_err(|e| format!("MLMultiArray: {}", e.localizedDescription()))?;
            let a = Self::wrap(inner);
            // first-major contiguous: zero the whole buffer
            let bytes = a.count() * 4;
            std::ptr::write_bytes(a.inner.dataPointer().as_ptr() as *mut u8, 0, bytes);
            Ok(a)
        }
    }

    pub fn zeros_f32(shape: &[usize]) -> Result<Self, String> {
        Self::alloc(shape, MLMultiArrayDataType::Float32)
    }

    pub fn zeros_i32(shape: &[usize]) -> Result<Self, String> {
        Self::alloc(shape, MLMultiArrayDataType::Int32)
    }

    pub fn wrap(inner: Retained<MLMultiArray>) -> Self {
        unsafe {
            let shape = read_numbers(&inner.shape());
            let strides = read_numbers(&inner.strides());
            Self { inner, shape, strides }
        }
    }

    pub fn count(&self) -> usize {
        self.shape.iter().product()
    }

    /// Contiguous view. Only valid for arrays we allocated (first-major,
    /// contiguous) or outputs whose strides are dense.
    pub fn f32_mut(&self) -> &mut [f32] {
        unsafe {
            std::slice::from_raw_parts_mut(self.inner.dataPointer().as_ptr() as *mut f32, self.count())
        }
    }

    pub fn i32_mut(&self) -> &mut [i32] {
        unsafe {
            std::slice::from_raw_parts_mut(self.inner.dataPointer().as_ptr() as *mut i32, self.count())
        }
    }

    pub fn ptr_f32(&self) -> *const f32 {
        unsafe { self.inner.dataPointer().as_ptr() as *const f32 }
    }

    pub fn is_contiguous(&self) -> bool {
        let mut expect = 1;
        for (s, st) in self.shape.iter().rev().zip(self.strides.iter().rev()) {
            if *st != expect {
                return false;
            }
            expect *= s;
        }
        true
    }
}

pub fn provider(items: &[(&str, &Array)]) -> Result<Retained<MLDictionaryFeatureProvider>, String> {
    let keys: Vec<Retained<NSString>> = items.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let vals: Vec<Retained<MLFeatureValue>> = items
        .iter()
        .map(|(_, a)| unsafe { MLFeatureValue::featureValueWithMultiArray(&a.inner) })
        .collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let val_refs: Vec<&AnyObject> = vals.iter().map(|v| -> &AnyObject { v }).collect();
    let dict: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&key_refs, &val_refs);
    unsafe {
        MLDictionaryFeatureProvider::initWithDictionary_error(
            MLDictionaryFeatureProvider::alloc(),
            &dict,
        )
        .map_err(|e| format!("provider: {}", e.localizedDescription()))
    }
}

pub fn output(f: &Features, name: &str) -> Result<Array, String> {
    unsafe {
        let fv = f
            .featureValueForName(&NSString::from_str(name))
            .ok_or_else(|| format!("output {name} missing"))?;
        let ma = fv
            .multiArrayValue()
            .ok_or_else(|| format!("output {name} is not a multiarray"))?;
        Ok(Array::wrap(ma))
    }
}
