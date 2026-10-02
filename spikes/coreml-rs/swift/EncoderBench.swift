// Reference: same Encoder.mlmodelc prediction from Swift, for the Rust-vs-Swift comparison.
// swiftc -O -o encoder-bench swift/EncoderBench.swift && ./encoder-bench <models-dir> [units] [iters]
import CoreML
import Foundation

let args = CommandLine.arguments
let dir = args.count > 1 ? args[1] : "."
let unitsName = args.count > 2 ? args[2] : "all"
let iters = args.count > 3 ? Int(args[3]) ?? 20 : 20
let units: MLComputeUnits = ["cpu": .cpuOnly, "cpu-gpu": .cpuAndGPU, "cpu-ane": .cpuAndNeuralEngine][unitsName] ?? .all

let config = MLModelConfiguration()
config.computeUnits = units
let t0 = Date()
let model = try MLModel(contentsOf: URL(fileURLWithPath: "\(dir)/Encoder.mlmodelc"), configuration: config)
let load = Date().timeIntervalSince(t0)
let mel = try MLMultiArray(shape: [1, 128, 1501], dataType: .float32)
let melLen = try MLMultiArray(shape: [1], dataType: .int32)
melLen[0] = 1501
let input = try MLDictionaryFeatureProvider(dictionary: [
    "mel": MLFeatureValue(multiArray: mel), "mel_length": MLFeatureValue(multiArray: melLen),
])
let t1 = Date()
let first = try model.prediction(from: input)
let firstMs = Date().timeIntervalSince(t1) * 1000
let encLen = first.featureValue(for: "encoder_length")!.multiArrayValue![0].intValue
var times: [Double] = []
for _ in 0..<iters {
    let t = Date()
    _ = try model.prediction(from: input)
    times.append(Date().timeIntervalSince(t) * 1000)
}
times.sort()
let mean = times.reduce(0, +) / Double(times.count)
print(String(format: "swift encoder units=%@ load=%.3fs first_call=%.1fms steady(n=%d) min=%.1fms median=%.1fms mean=%.1fms max=%.1fms encoder_length=%d",
    unitsName, load, firstMs, iters, times[0], times[times.count / 2], mean, times[times.count - 1], encLen))
