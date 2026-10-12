import { ExternalLinkIcon, TriangleAlertIcon } from "lucide-react";
import type { Acknowledgement } from "@/bridge/contract";
import { send, useBridge, useSnapshot } from "@/bridge/hooks";
import { PermissionRow } from "@/components/permission-row";
import {
	Button,
	Callout,
	Dialog,
	DialogBody,
	DialogClose,
	DialogCloseButton,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogPopup,
	DialogTitle,
	DialogTrigger,
	Disclosure,
	FormCard,
	FormRow,
	FormValue,
	ScrollArea,
	Select,
	Switch,
} from "@/components/ui";
import { usePlatform } from "@/lib/platform";
import { SectionPage } from "./section-page";
import { updateStatusText } from "./settings-format";

/** The speech models and libraries Steno ships with, and their licences. */
function AcknowledgementsDialog({
	acknowledgements,
}: {
	acknowledgements: readonly Acknowledgement[];
}) {
	const client = useBridge();
	const groups: { title: string; group: Acknowledgement["group"] }[] = [
		{ title: "Speech models", group: "speechModels" },
		{ title: "Libraries", group: "libraries" },
	];
	return (
		<Dialog>
			<DialogTrigger
				render={
					<Button data-testid="acknowledgements" size="sm" variant="ghost" />
				}
			>
				Acknowledgements…
			</DialogTrigger>
			<DialogPopup className="max-w-lg" data-testid="acknowledgements-dialog">
				<DialogHeader>
					<DialogTitle>Acknowledgements</DialogTitle>
					<DialogDescription>
						The speech models and libraries Steno is built on.
					</DialogDescription>
				</DialogHeader>
				<DialogBody>
					<ScrollArea viewportClassName="max-h-80">
						<div className="flex flex-col gap-4">
							{groups.map(({ title, group }) => (
								<FormCard key={group} title={title}>
									{acknowledgements
										.filter((item) => item.group === group)
										.map((item) => (
											<FormRow
												control={
													<FormValue variant="faint">{item.licence}</FormValue>
												}
												description={
													item.source.startsWith("https://") ? (
														<Button
															onClick={() =>
																send(client, "system.openURL", {
																	url: item.source,
																})
															}
															size="xs"
															variant="ghost"
														>
															{item.source.replace("https://", "")}
															<ExternalLinkIcon aria-hidden="true" />
														</Button>
													) : (
														item.source
													)
												}
												key={item.name}
												label={item.name}
											/>
										))}
								</FormCard>
							))}
						</div>
					</ScrollArea>
				</DialogBody>
				<DialogFooter>
					<DialogClose render={<Button variant="primary" />}>Done</DialogClose>
				</DialogFooter>
				<DialogCloseButton />
			</DialogPopup>
		</Dialog>
	);
}

/**
 * General: launch at login, meeting detection, the calendar permission that
 * names meetings (where the platform has one Steno reads), the default
 * template, the update status and the acknowledgements.
 */
export function GeneralSection() {
	const client = useBridge();
	const platform = usePlatform();
	const general = useSnapshot("settings.general");
	if (!general) {
		return <SectionPage id="general" />;
	}
	const launchAtLogin =
		general.loginItem === "enabled" || general.loginItem === "requiresApproval";
	const template = general.templates.find(
		(candidate) => candidate.id === general.defaultTemplateID,
	);
	const updates = general.updates;

	return (
		<SectionPage
			error={general.error}
			errorDetails={general.errorDetails}
			id="general"
		>
			<FormCard>
				<FormRow
					control={
						<Switch
							aria-label="Open Steno at login"
							checked={launchAtLogin}
							data-testid="launch-at-login"
							disabled={general.loginItemNote !== undefined}
							onCheckedChange={(value) =>
								send(client, "settings.general.setLaunchAtLogin", { value })
							}
						/>
					}
					description={general.loginItemNote}
					label="Open Steno at login"
				>
					{general.loginItem === "requiresApproval" ? (
						<Callout
							actions={
								<Button
									data-testid="open-login-items"
									onClick={() =>
										send(client, "settings.general.openLoginItems")
									}
									size="sm"
									variant="outline"
								>
									Open Login Items
								</Button>
							}
							data-testid="login-item-approval"
							icon={<TriangleAlertIcon aria-hidden="true" />}
							size="sm"
							title="Waiting for your approval in System Settings › Login Items."
							variant="warning"
						/>
					) : null}
				</FormRow>
				<FormRow
					control={
						<Switch
							aria-label="Offer to record when a call starts"
							checked={general.detectionEnabled}
							data-testid="detection"
							onCheckedChange={(value) =>
								send(client, "settings.general.setDetectionEnabled", { value })
							}
						/>
					}
					description="Steno notices when another app opens the microphone and asks before recording."
					label="Offer to record when a call starts"
				/>
			</FormCard>

			<FormCard title="Meetings">
				{platform.readsCalendar ? (
					<PermissionRow
						isRequesting={general.requestingCalendar}
						kind="calendar"
						onOpenSystemSettings={() =>
							send(client, "system.openSystemSettings", { kind: "calendar" })
						}
						onRequest={() => send(client, "settings.general.requestCalendar")}
						state={general.calendarPermission}
					/>
				) : null}
				<FormRow
					control={
						<Select
							aria-label="Summary template"
							className="w-40"
							data-testid="default-template"
							onValueChange={(templateID) => {
								if (templateID) {
									send(client, "settings.general.setDefaultTemplate", {
										templateID,
									});
								}
							}}
							options={general.templates.map((candidate) => ({
								value: candidate.id,
								label: candidate.name,
							}))}
							size="sm"
							value={general.defaultTemplateID}
						/>
					}
					description={template?.description}
					label="Summary template"
				/>
			</FormCard>

			{updates.managedNote ? (
				<FormCard title="Updates">
					<FormRow
						description={
							<span data-testid="update-status">{updates.managedNote}</span>
						}
						label={`Steno ${general.version}`}
					/>
				</FormCard>
			) : (
				<FormCard footer="Updates are checked once a day." title="Updates">
					<FormRow
						control={
							<Button
								data-testid="check-for-updates"
								disabled={!updates.canCheck}
								onClick={() => send(client, "updates.check")}
								size="sm"
								variant="outline"
							>
								Check for Updates
							</Button>
						}
						description={
							<span data-testid="update-status">
								{updateStatusText(updates)}
							</span>
						}
						label={`Steno ${general.version}`}
					>
						{updates.outcome === "failed" && updates.detail ? (
							<Disclosure data-testid="update-failure">
								{updates.detail}
							</Disclosure>
						) : null}
					</FormRow>
					<FormRow
						control={
							<Switch
								aria-label="Check for updates automatically"
								checked={updates.automaticallyChecks}
								data-testid="auto-check"
								onCheckedChange={(automaticallyChecks) =>
									send(client, "settings.general.setAutomaticUpdates", {
										automaticallyChecks,
										automaticallyDownloads: updates.automaticallyDownloads,
									})
								}
							/>
						}
						label="Check for updates automatically"
					/>
					<FormRow
						control={
							<Switch
								aria-label="Install updates automatically"
								checked={updates.automaticallyDownloads}
								data-testid="auto-install"
								onCheckedChange={(automaticallyDownloads) =>
									send(client, "settings.general.setAutomaticUpdates", {
										automaticallyChecks: updates.automaticallyChecks,
										automaticallyDownloads,
									})
								}
							/>
						}
						label="Install updates automatically"
					/>
				</FormCard>
			)}

			<div className="flex">
				<AcknowledgementsDialog acknowledgements={general.acknowledgements} />
			</div>
		</SectionPage>
	);
}
