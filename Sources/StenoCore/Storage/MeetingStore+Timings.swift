import Foundation
import GRDB

extension MeetingStore {
  /// The seeds with every learned rate laid over them. A row whose stage is
  /// unknown, or whose key is not empty for a stage that is not keyed by an
  /// engine or a model, is ignored.
  public func stageRates() async throws -> StageRates {
    try await writer.read { db in
      var rates = StageRates.seeds
      for row in try StageRateRow.fetchAll(db) {
        guard let stage = PipelineStage(rawValue: row.stage),
          StageRates.isKeyed(stage) || row.key == StageRates.unkeyed
        else { continue }
        rates.set(row.rate, stage, key: row.key)
      }
      return rates
    }
  }

  /// Folds one measurement into its row inside one write, per
  /// `StageRate.absorbing`: the first sample replaces the seed outright,
  /// later ones move the average.
  public func record(_ sample: StageSample) async throws {
    try await writer.write { db in
      let current =
        try StageRateRow.fetchOne(db, key: ["stage": sample.stage.rawValue, "key": sample.key])?
        .rate ?? StageRates.seeds.rate(sample.stage, key: sample.key)
      try StageRateRow(
        stage: sample.stage, key: sample.key, rate: current.absorbing(sample.secondsPerUnit),
        updatedAt: sample.recordedAt
      ).save(db)
    }
  }
}
