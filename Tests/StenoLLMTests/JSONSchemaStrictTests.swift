import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// Walks the literal JSON every request-side schema emits and asserts the
/// strict subset OpenAI, LM Studio and Groq agree on: every object has
/// `additionalProperties: false` and lists every property in `required`,
/// containers nest at most five levels deep (leaves do not count), and no
/// banned keyword appears.
@Suite struct JSONSchemaStrictTests {
  static let banned: Set<String> = [
    "format", "pattern", "minLength", "maxLength", "minimum", "maximum", "minItems", "maxItems",
    "anyOf", "oneOf", "allOf", "default", "$ref", "uniqueItems", "patternProperties",
  ]

  /// Every violation of the strict subset in `value`, empty when compliant.
  static func problems(in value: JSONValue, path: String = "root", depth: Int = 1) -> [String] {
    guard case .object(let object) = value else { return ["\(path): schema node is not an object"] }
    var problems: [String] = []
    for key in object.keys.sorted() where banned.contains(key) {
      problems.append("\(path): banned keyword \(key)")
    }
    let type: String?
    switch object["type"] {
    case .string(let name)?:
      type = name
    case .array(let names)?:
      let strings = names.compactMap {
        if case .string(let s) = $0 { return s } else { return nil }
      }
      if strings.count != 2 || !strings.contains("null") {
        problems.append("\(path): type array must be [type, null]")
      }
      type = strings.first { $0 != "null" }
    default:
      problems.append("\(path): missing type")
      type = nil
    }
    if depth > 5, type == "object" || type == "array" {
      problems.append("\(path): nesting depth \(depth) exceeds 5")
    }
    switch type {
    case "object":
      if object["additionalProperties"] != .bool(false) {
        problems.append("\(path): additionalProperties must be false")
      }
      guard case .object(let properties)? = object["properties"] else {
        return problems + ["\(path): object without properties"]
      }
      guard case .array(let required)? = object["required"] else {
        return problems + ["\(path): object without required"]
      }
      let requiredNames = Set(
        required.compactMap { if case .string(let s) = $0 { return s } else { return nil } })
      if requiredNames != Set(properties.keys) {
        problems.append("\(path): every property must be required")
      }
      for (name, child) in properties.sorted(by: { $0.key < $1.key }) {
        problems += self.problems(in: child, path: "\(path).\(name)", depth: depth + 1)
      }
    case "array":
      guard let items = object["items"] else { return problems + ["\(path): array without items"] }
      problems += self.problems(in: items, path: "\(path)[]", depth: depth + 1)
    case "string", "integer", "number", "boolean":
      break
    default:
      problems.append("\(path): unexpected type \(String(describing: type))")
    }
    return problems
  }

  static let sample = JSONSchema.object(
    [
      "title": .string(description: "under 80 characters"),
      "count": .integer(),
      "score": .number().nullable,
      "flag": .boolean(),
      "kind": .string(enum: ["a", "b"]),
      "items": .array(
        of: .object([
          "id": .string(), "note": .string().nullable,
          "tags": .array(of: .string()),
        ])),
    ], description: "sample")

  @Test func builderEmitsTheStrictSubset() {
    #expect(Self.problems(in: Self.sample.jsonValue) == [])
    let json = Self.sample.jsonValue
    #expect(json["type"] == "object")
    #expect(json["description"] == "sample")
    #expect(json["additionalProperties"] == false)
    #expect(json["properties"]?["score"]?["type"] == .array(["number", "null"]))
    #expect(json["properties"]?["kind"]?["enum"] == .array(["a", "b"]))
    #expect(json["properties"]?["items"]?["items"]?["required"]?.arrayValue?.count == 3)
    #expect(
      Self.sample.properties.map(\.name) == ["title", "count", "score", "flag", "kind", "items"])
  }

  @Test func theProbeSchemaIsStrictToo() {
    #expect(Self.problems(in: OpenAICompatibleClient.probeSchema.jsonValue) == [])
    guard
      case .jsonSchema(let name, let schema, let strict) =
        OpenAICompatibleClient.probeRequest.responseFormat
    else {
      Issue.record("the probe asks for a JSON schema")
      return
    }
    #expect(name == "probe")
    #expect(strict)
    #expect(schema == OpenAICompatibleClient.probeSchema.jsonValue)
    #expect(schema["properties"]?["ok"]?["type"] == "boolean")
  }

  @Test func walkerRejectsLooseSchemas() {
    let loose: JSONValue = [
      "type": "object", "properties": ["a": ["type": "string", "format": "date"]],
      "required": [], "additionalProperties": true,
    ]
    let problems = Self.problems(in: loose)
    #expect(problems.contains("root: additionalProperties must be false"))
    #expect(problems.contains("root: every property must be required"))
    #expect(problems.contains("root.a: banned keyword format"))
    var deep: JSONValue = ["type": "string"]
    for _ in 0..<6 {
      deep = [
        "type": "object", "properties": ["x": deep], "required": ["x"],
        "additionalProperties": false,
      ]
    }
    #expect(Self.problems(in: deep).contains { $0.contains("exceeds 5") })
  }

  @Test func promptTextIsCompactAndOrdered() {
    let expected = """
      {
        "title": string,  // under 80 characters
        "count": integer,
        "score": number | null,
        "flag": boolean,
        "kind": "a" | "b",
        "items": [{
          "id": string,
          "note": string | null,
          "tags": [string]
        }]
      }
      """
    #expect(Self.sample.promptText == expected)
    #expect(
      JSONSchema.object(["lead": .string(), "text": .string()]).promptText
        == "{ \"lead\": string, \"text\": string }")
  }
}

extension JSONValue {
  fileprivate var arrayValue: [JSONValue]? {
    if case .array(let array) = self { return array }
    return nil
  }
}
