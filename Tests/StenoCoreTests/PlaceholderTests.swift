import GRDB
import Testing

@testable import StenoCore

@Test func placeholderModuleCompiles() {
  #expect(StenoCore.version.isEmpty == false)
}

@Test func fts5IsAvailableInTheSystemSQLite() throws {
  let queue = try DatabaseQueue()
  try queue.write { db in
    try db.execute(sql: "CREATE VIRTUAL TABLE t USING fts5(x)")
    try db.execute(sql: "INSERT INTO t(x) VALUES ('hello world')")
    let count = try Int.fetchOne(db, sql: "SELECT count(*) FROM t WHERE t MATCH 'hello'")
    #expect(count == 1)
  }
}
