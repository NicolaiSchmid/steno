import {
	CheckCircle2Icon,
	ChevronDownIcon,
	CircleAlertIcon,
	ClockIcon,
	CopyIcon,
	FolderIcon,
	InboxIcon,
	MoreHorizontalIcon,
	PlayIcon,
	RefreshCwIcon,
	SettingsIcon,
	ShareIcon,
	SmartphoneIcon,
	SparklesIcon,
	Trash2Icon,
} from "lucide-react";
import { Fragment, useState } from "react";
import {
	Avatar,
	AvatarStack,
	Badge,
	Button,
	Callout,
	Card,
	Checkbox,
	Menu,
	MenuItem,
	MenuPopup,
	MenuSeparator,
	MenuTrigger,
	Pill,
	RecordMark,
	ScrollArea,
	SearchInput,
	SectionLabel,
	SidebarRow,
	Tabs,
	TabsList,
	TabsPanel,
	TabsTab,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import {
	days,
	detail,
	filters,
	highlightSegments,
	type SamplePerson,
	tags,
} from "./sample-data";

export type ShellTab = "summary" | "transcript" | "tasks" | "notes";

export interface AppShellPreviewProps {
	tab?: ShellTab;
	menuOpen?: boolean;
}

const filterIcons = {
	all: <InboxIcon />,
	progress: <ClockIcon />,
	ready: <CheckCircle2Icon />,
	failed: <CircleAlertIcon />,
} as const;

function People({
	people,
	size,
	ring,
}: {
	people: SamplePerson[];
	size: "sm" | "md";
	ring: "background" | "card";
}) {
	return (
		<AvatarStack ring={ring}>
			{people.map((person) => (
				<Avatar
					index={person.index}
					key={person.name}
					name={person.name}
					size={size}
					unknown={person.unknown ?? false}
				/>
			))}
		</AvatarStack>
	);
}

/**
 * The mockup's main window rebuilt from the ui kit with sample data. Sidebar,
 * meeting list and detail; the actions menu opens with `menuOpen`.
 */
export function AppShellPreview({
	tab: initialTab = "summary",
	menuOpen: initialMenuOpen = false,
}: AppShellPreviewProps) {
	const [tab, setTab] = useState<ShellTab>(initialTab);
	const [menuOpen, setMenuOpen] = useState(initialMenuOpen);
	const [selected, setSelected] = useState("m1");
	const [filter, setFilter] = useState<string>("all");

	return (
		<div
			className="grid h-full min-h-0 grid-cols-[236px_320px_minmax(0,1fr)] overflow-hidden bg-background text-foreground"
			data-testid="shell"
		>
			<aside className="surface-grain flex min-h-0 flex-col gap-0.5 border-border border-r bg-sidebar px-2 pt-[52px] pb-2">
				<Button
					className="mb-3 h-9 w-full justify-start"
					size="lg"
					variant="primary"
				>
					<RecordMark />
					Record
					<ChevronDownIcon aria-hidden="true" className="ml-auto size-3.5" />
				</Button>
				<SectionLabel>Meetings</SectionLabel>
				{filters.map((item) => (
					<SidebarRow
						active={filter === item.id}
						count={item.count}
						icon={filterIcons[item.id]}
						key={item.id}
						onClick={() => setFilter(item.id)}
					>
						{item.label}
					</SidebarRow>
				))}
				<SectionLabel>Tags</SectionLabel>
				{tags.map((tag) => (
					<SidebarRow key={tag} variant="tag">
						{tag}
					</SidebarRow>
				))}
				<div className="mt-auto flex flex-col gap-1.5">
					<Card className="flex items-center gap-[9px]" padding="sm">
						<SmartphoneIcon
							aria-hidden="true"
							className="size-4 shrink-0 stroke-[1.75] text-muted-foreground"
						/>
						<span className="min-w-0 flex-1 text-muted-foreground text-xs">
							<span className="block font-medium text-[12.5px] text-foreground">
								Nicolai's iPhone
							</span>
							Synced 2 min ago
						</span>
						<span
							aria-label="Connected"
							className="size-[7px] shrink-0 rounded-full bg-primary-2 shadow-[0_0_0_3px_var(--primary-soft)]"
							role="img"
						/>
					</Card>
					<SidebarRow icon={<SettingsIcon />}>Settings</SidebarRow>
				</div>
			</aside>

			<section className="flex min-h-0 flex-col overflow-hidden border-border border-r">
				<header className="flex flex-col gap-2.5 px-3 pt-[52px] pb-2">
					<h1 className="mx-1 my-0 font-semibold text-[15px]">Meetings</h1>
					<SearchInput aria-label="Search meetings" shortcut="⌘F" />
				</header>
				<ScrollArea className="flex-1">
					{days.map((day) => (
						<Fragment key={day.label}>
							<div className="px-4 pt-3.5 pb-1.5 font-medium text-[11px] text-faint">
								{day.label}
								<span className="ml-1.5 font-normal text-muted-foreground">
									{day.date}
								</span>
							</div>
							{day.meetings.map((meeting) => {
								const active = meeting.id === selected;
								return (
									<button
										aria-current={active ? "true" : undefined}
										className={[
											"mx-2 mb-1 block w-[calc(100%-16px)] rounded-lg border px-[11px] pt-[9px] pb-2.5 text-left outline-none transition-colors duration-(--duration-functional) ease-standard focus-visible:ring-2 focus-visible:ring-primary/50",
											active
												? "border-border bg-card shadow-xs"
												: "border-transparent hover:bg-accent",
										].join(" ")}
										key={meeting.id}
										onClick={() => setSelected(meeting.id)}
										type="button"
									>
										<div className="flex items-baseline gap-2.5 font-medium text-[13px]">
											<span className="min-w-0 flex-1 truncate">
												{meeting.title}
											</span>
											<time className="font-mono text-[11px] text-faint tabular-nums">
												{meeting.time}
											</time>
										</div>
										<p className="my-0 mt-[3px] line-clamp-2 text-muted-foreground text-xs leading-[1.45]">
											{meeting.preview}
										</p>
										<div className="mt-2 flex items-center gap-2 text-[11px] text-faint">
											<Badge>{meeting.kind}</Badge>
											<span className="font-mono tabular-nums">
												{meeting.duration}
											</span>
											{meeting.noSummary ? (
												<Badge variant="warn">No summary</Badge>
											) : null}
											{meeting.people.length > 0 ? (
												<People
													people={meeting.people}
													ring={active ? "card" : "background"}
													size="sm"
												/>
											) : null}
										</div>
									</button>
								);
							})}
						</Fragment>
					))}
				</ScrollArea>
			</section>

			<main className="surface-grain relative flex min-h-0 min-w-0 flex-col">
				<div className="pointer-events-none absolute inset-x-0 top-0 z-10 flex h-[52px] items-center justify-end gap-2 px-3.5 [&>*]:pointer-events-auto">
					<Button variant="glass">
						<ShareIcon aria-hidden="true" />
						Export
					</Button>
					<Menu onOpenChange={setMenuOpen} open={menuOpen}>
						<MenuTrigger
							render={
								<Button aria-label="More actions" size="icon" variant="glass" />
							}
						>
							<MoreHorizontalIcon aria-hidden="true" />
						</MenuTrigger>
						<MenuPopup align="end" sideOffset={4}>
							<MenuItem icon={<RefreshCwIcon />}>Re-run summary</MenuItem>
							<MenuItem icon={<ShareIcon />} shortcut="⇧⌘E">
								Export again
							</MenuItem>
							<MenuItem icon={<FolderIcon />}>Reveal recording</MenuItem>
							<MenuSeparator />
							<MenuItem icon={<Trash2Icon />} variant="destructive">
								Delete meeting…
							</MenuItem>
						</MenuPopup>
					</Menu>
				</div>
				<ScrollArea className="flex-1">
					<div className="max-w-[720px] px-10 pt-[60px] pb-12">
						<Callout
							actions={
								<>
									<Button size="sm" variant="primary">
										Set up
									</Button>
									<Button size="sm" variant="ghost">
										Not now
									</Button>
								</>
							}
							className="mb-[26px]"
							description="Choose an AI service and Steno writes a summary and tasks for every meeting. Only the transcript text is sent."
							icon={<SparklesIcon aria-hidden="true" />}
							title="Summaries are off."
						/>
						<div className="flex items-center gap-2 text-faint text-xs [&>i]:size-[3px] [&>i]:rounded-full [&>i]:bg-border">
							<span className="inline-flex items-center gap-[5px] font-medium text-muted-foreground before:size-1.5 before:rounded-full before:bg-p4 before:content-['']">
								{detail.kind}
							</span>
							<i />
							<span>{detail.when}</span>
							<i />
							<span className="font-mono tabular-nums">{detail.duration}</span>
							<i />
							<span>{detail.language}</span>
							<i />
							<span>{detail.retention}</span>
						</div>
						<h2 className="mt-2 mb-3.5 font-semibold text-[26px] leading-[1.2] tracking-[-0.02em]">
							{detail.title}
						</h2>
						<div className="mb-6 flex flex-wrap items-center gap-2.5">
							<People people={detail.people} ring="background" size="md" />
							<span className="text-[13px] text-muted-foreground">
								{detail.names}
							</span>
							<Pill onClick={() => undefined} variant="live">
								<CheckCircle2Icon aria-hidden="true" />
								Confirm speaker
							</Pill>
							{detail.tags.map((tag) => (
								<Pill key={tag}>#{tag}</Pill>
							))}
						</div>

						<Tabs
							onValueChange={(value) => setTab(value as ShellTab)}
							value={tab}
						>
							<TabsList className="mb-[26px]">
								<TabsTab value="summary">Summary</TabsTab>
								<TabsTab count={detail.turnCount} value="transcript">
									Transcript
								</TabsTab>
								<TabsTab count={detail.taskCount} value="tasks">
									Tasks
								</TabsTab>
								<TabsTab value="notes">Notes</TabsTab>
							</TabsList>

							<TabsPanel value="summary" variant="reading">
								{detail.summary.map((section) => (
									<Fragment key={section.heading}>
										<h3 className="mt-[26px] mb-2 font-semibold text-base first:mt-0">
											{section.heading}
										</h3>
										<ul className="my-0 list-disc pl-[22px]">
											{section.items.map((item) => (
												<li className="my-1.5" key={item.lead}>
													<b className="font-semibold">{item.lead}</b>{" "}
													{item.text}
													{item.timestamp ? (
														<span className="ml-2 font-mono text-[11.5px] text-faint">
															{item.timestamp}
														</span>
													) : null}
												</li>
											))}
										</ul>
									</Fragment>
								))}
								<h3 className="mt-[26px] mb-2 font-semibold text-base">
									Tasks
								</h3>
								{detail.tasks.map((task) => (
									<Card
										className="my-2 flex items-start gap-3"
										key={task.id}
										padding="md"
									>
										<Checkbox
											aria-label={task.text}
											className="mt-0.5"
											defaultChecked={task.done}
										/>
										<span
											className={cn(
												"text-sm leading-[1.4]",
												task.done && "text-faint line-through",
											)}
										>
											{task.text}
											<small className="mt-0.5 block text-faint text-xs no-underline">
												{task.meta}
											</small>
										</span>
									</Card>
								))}
							</TabsPanel>

							<TabsPanel value="transcript">
								<div className="mb-[26px] flex items-center gap-[18px] text-[13px] text-muted-foreground">
									<Button size="sm" variant="ghost">
										<CopyIcon aria-hidden="true" />
										Copy transcript
									</Button>
									<Button size="sm" variant="ghost">
										<PlayIcon aria-hidden="true" />
										Play from here
									</Button>
								</div>
								{detail.turns.map((turn) => (
									<div
										className="mb-[22px] grid grid-cols-[140px_1fr] gap-[18px] text-[15px] leading-[1.6]"
										key={turn.id}
									>
										<div className="flex flex-col gap-[3px] pt-0.5 font-medium text-[13.5px]">
											{turn.who}
											<span className="font-mono font-normal text-[11.5px] text-faint">
												{turn.range}
											</span>
											{turn.unnamed ? (
												<Badge className="mt-1 self-start" variant="warn">
													Who is this?
												</Badge>
											) : null}
										</div>
										<p className="my-0">
											{highlightSegments(turn.text, turn.highlight).map(
												(segment) =>
													segment.marked ? (
														<mark
															className="rounded-[3px] bg-[rgb(250_204_21/45%)] px-0.5 text-inherit dark:bg-[rgb(250_204_21/25%)]"
															key={`${turn.id}-${segment.offset}`}
														>
															{segment.text}
														</mark>
													) : (
														<Fragment key={`${turn.id}-${segment.offset}`}>
															{segment.text}
														</Fragment>
													),
											)}
										</p>
									</div>
								))}
							</TabsPanel>

							<TabsPanel value="tasks" variant="reading">
								<p className="my-0 text-muted-foreground">
									Three tasks, one done. The full list comes with WP2.
								</p>
							</TabsPanel>
							<TabsPanel value="notes" variant="reading">
								<p className="my-0 text-muted-foreground">
									Your notes for this meeting.
								</p>
							</TabsPanel>
						</Tabs>
					</div>
				</ScrollArea>
			</main>
		</div>
	);
}
