# Summary templates

One JSON file per bundled `SummaryTemplate` (`default`, `customer-discovery`,
`daily-standup`, `interview`), loaded through `Bundle.module` by
`SummaryTemplate.bundled`. Section `id` and `heading` values are fixed by the
core foundation; the `instructions` and `context` strings are prompt text that
the LLM workstream edits in PRs against this module.
