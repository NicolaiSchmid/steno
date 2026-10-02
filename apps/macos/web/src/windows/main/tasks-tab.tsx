import { ListChecksIcon } from "lucide-react";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { Badge, Card, Checkbox, EmptyState } from "@/components/ui";
import { cn } from "@/lib/cn";
import { format } from "./format";

export type Task = MeetingDetailSnapshot["tasks"][number];

/** One task as a card: the box, the text, who and when beneath. */
export function TaskCard({ task }: { task: Task }) {
	const meta = [
		task.assigneeName,
		task.dueDate ? format.dueDate(task.dueDate) : undefined,
	].filter((part): part is string => Boolean(part));
	return (
		<Card className="my-2 flex items-start gap-3" padding="md">
			<Checkbox
				aria-label={task.text}
				checked={task.done}
				className="mt-0.5"
				readOnly
			/>
			<span
				className={cn(
					"min-w-0 flex-1 text-sm leading-5",
					task.done && "text-faint line-through",
				)}
			>
				{task.text}
				{meta.length > 0 || task.priority === "high" ? (
					<small className="mt-0.5 flex items-center gap-2 text-faint text-xs no-underline">
						{meta.length > 0 ? <span>{meta.join(" · ")}</span> : null}
						{task.priority === "high" ? (
							<Badge size="sm" variant="warning">
								High priority
							</Badge>
						) : null}
					</small>
				) : null}
			</span>
		</Card>
	);
}

export interface TasksTabProps {
	tasks: readonly Task[];
}

export function TasksTab({ tasks }: TasksTabProps) {
	if (tasks.length === 0) {
		return (
			<EmptyState
				body="No tasks were found in this meeting."
				icon={<ListChecksIcon aria-hidden="true" />}
				id="empty-tasks"
				title="No tasks"
			/>
		);
	}
	return (
		<div data-testid="tab-content-tasks">
			{tasks.map((task) => (
				<TaskCard key={task.id} task={task} />
			))}
		</div>
	);
}
