//! The tasks note in Obsidian Tasks syntax.
//! Swift: `Sources/StenoAdapters/Rendering/TasksMarkdownRenderer.swift`.

use steno_core::{MeetingExport, MeetingTask, TaskPriority};

use super::{ArtifactRenderer, Names, RenderOptions, date_text, markdown_text};

/// The plugin reads its fields from the end of the line, so the order is
/// fixed: description, assignee link, tag, priority, due date. Emoji are
/// bare code points: U+FE0F and NBSP break recognition.
pub(crate) struct TasksMarkdownRenderer<'a> {
    pub export: &'a MeetingExport,
    pub options: &'a RenderOptions,
}

impl TasksMarkdownRenderer<'_> {
    pub const HIGH_PRIORITY: &'static str = "\u{23EB}"; // ⏫
    pub const LOW_PRIORITY: &'static str = "\u{1F53D}"; // 🔽
    pub const DUE_MARKER: &'static str = "\u{1F4C5}"; // 📅
    pub const CLOSING_LINE: &'static str =
        "Edit tasks in Steno; this file is rewritten on re-export.";

    pub fn render(&self) -> String {
        let mut parts = ArtifactRenderer::note_head(self.export, "Tasks", self.options.time_zone);
        if self.export.tasks.is_empty() {
            parts.push("No tasks.\n".to_owned());
        } else {
            let mut lines = self
                .export
                .tasks
                .iter()
                .map(|task| self.line(task))
                .collect::<Vec<_>>()
                .join("\n");
            lines.push('\n');
            parts.push(lines);
        }
        parts.push(format!("{}\n", Self::CLOSING_LINE));
        parts.join("\n")
    }

    /// `- [ ] Angebot an ACME schicken [[Anna Müller]] #task ⏫ 📅 2026-10-01`.
    pub fn line(&self, task: &MeetingTask) -> String {
        let mut fields = vec![format!(
            "- [{}] {}",
            if task.done { "x" } else { " " },
            markdown_text::single_line(&task.text)
        )];
        let names = Names {
            export: self.export,
            options: self.options,
        };
        if let Some(assignee) = names.assignee(task) {
            fields.push(markdown_text::single_line(&assignee));
        }
        if let Some(tag) = self
            .options
            .task_tag
            .as_deref()
            .and_then(markdown_text::tag)
        {
            fields.push(format!("#{tag}"));
        }
        match task.priority {
            TaskPriority::High => fields.push(Self::HIGH_PRIORITY.to_owned()),
            TaskPriority::Low => fields.push(Self::LOW_PRIORITY.to_owned()),
            TaskPriority::Normal => {}
        }
        if let Some(due_date) = task.due_date {
            fields.push(format!(
                "{} {}",
                Self::DUE_MARKER,
                date_text::day(due_date, self.options.time_zone)
            ));
        }
        fields.join(" ")
    }
}
