import Foundation
import StenoCore

/// A request-side JSON Schema builder limited to what every structured
/// output implementation accepts (OpenAI's strict subset): objects with
/// every property required and `additionalProperties: false`, arrays,
/// strings with optional `enum`, integers, numbers, booleans, and nullable
/// variants as `"type": [..., "null"]`. No `format`, `pattern`, bounds or
/// `anyOf`. Validation of the model's answer is done by decoding into the
/// `Codable` draft types, never by this schema.
public struct JSONSchema: Sendable, Equatable {
  public struct Property: Sendable, Equatable {
    public var name: String
    public var schema: JSONSchema
  }

  indirect enum Node: Sendable, Equatable {
    case object([Property])
    case array(JSONSchema)
    case string(enumCases: [String]?)
    case integer
    case number
    case boolean
  }

  var node: Node
  var description: String?
  var isNullable = false

  public static func object(
    _ properties: KeyValuePairs<String, JSONSchema>, description: String? = nil
  ) -> JSONSchema {
    JSONSchema(
      node: .object(properties.map { Property(name: $0.key, schema: $0.value) }),
      description: description)
  }

  public static func array(of item: JSONSchema, description: String? = nil) -> JSONSchema {
    JSONSchema(node: .array(item), description: description)
  }

  public static func string(enum cases: [String]? = nil, description: String? = nil) -> JSONSchema {
    JSONSchema(node: .string(enumCases: cases), description: description)
  }

  public static func integer(description: String? = nil) -> JSONSchema {
    JSONSchema(node: .integer, description: description)
  }

  public static func number(description: String? = nil) -> JSONSchema {
    JSONSchema(node: .number, description: description)
  }

  public static func boolean(description: String? = nil) -> JSONSchema {
    JSONSchema(node: .boolean, description: description)
  }

  /// The same schema accepting `null`.
  public var nullable: JSONSchema {
    var copy = self
    copy.isNullable = true
    return copy
  }

  public var properties: [Property] {
    if case .object(let properties) = node { return properties }
    return []
  }

  // MARK: Wire form

  /// The schema as sent in `response_format.json_schema.schema`.
  public var jsonValue: JSONValue {
    var object: [String: JSONValue] = [:]
    let typeName: String
    switch node {
    case .object(let properties):
      typeName = "object"
      object["properties"] = .object(
        Dictionary(uniqueKeysWithValues: properties.map { ($0.name, $0.schema.jsonValue) }))
      object["required"] = .array(properties.map { .string($0.name) })
      object["additionalProperties"] = .bool(false)
    case .array(let item):
      typeName = "array"
      object["items"] = item.jsonValue
    case .string(let cases):
      typeName = "string"
      if let cases { object["enum"] = .array(cases.map(JSONValue.string)) }
    case .integer: typeName = "integer"
    case .number: typeName = "number"
    case .boolean: typeName = "boolean"
    }
    object["type"] = isNullable ? .array([.string(typeName), .string("null")]) : .string(typeName)
    if let description { object["description"] = .string(description) }
    return .object(object)
  }

  // MARK: Prompt form

  /// A compact, readable shape for the prompt: JSON with type names in
  /// place of values, `"a" | "b"` for enums, `| null` for nullable,
  /// descriptions as trailing comments. Shorter than the schema and easier
  /// for small models than JSON Schema itself.
  public var promptText: String {
    render(indent: 0)
  }

  private func render(indent: Int) -> String {
    let pad = String(repeating: "  ", count: indent)
    let inner = String(repeating: "  ", count: indent + 1)
    var text: String
    switch node {
    case .object(let properties):
      if properties.allSatisfy({ $0.schema.isScalar && $0.schema.description == nil })
        && properties.count <= 3
      {
        text = "{ "
        text += properties.map { "\"\($0.name)\": \($0.schema.render(indent: indent))" }
          .joined(separator: ", ")
        text += " }"
      } else {
        var lines: [String] = ["{"]
        for (offset, property) in properties.enumerated() {
          var line = "\(inner)\"\(property.name)\": \(property.schema.render(indent: indent + 1))"
          if offset < properties.count - 1 { line += "," }
          if let description = property.schema.description { line += "  // \(description)" }
          lines.append(line)
        }
        lines.append("\(pad)}")
        text = lines.joined(separator: "\n")
      }
    case .array(let item):
      let rendered = item.render(indent: indent)
      text = "[\(rendered)]"
    case .string(let cases):
      text = cases.map { $0.map { "\"\($0)\"" }.joined(separator: " | ") } ?? "string"
    case .integer: text = "integer"
    case .number: text = "number"
    case .boolean: text = "boolean"
    }
    if isNullable { text += " | null" }
    return text
  }

  private var isScalar: Bool {
    switch node {
    case .object, .array: false
    default: true
    }
  }
}
