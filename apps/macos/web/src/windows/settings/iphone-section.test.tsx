import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { PhoneSettingsSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { platformFor } from "@/lib/platform";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { PhoneSection } from "./iphone-section";

const PHONE_ID = "00000000-0000-0000-0000-000000000028";

describe("PhoneSection", () => {
	it("lists the paired phone and removes it through the host", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<PhoneSection />, harness);
		const row = screen.getByTestId(`device-${PHONE_ID}`);
		expect(row).toHaveTextContent("Nicolai's iPhone");
		expect(row).toHaveTextContent(/Paired .* · last seen /);
		expect(screen.queryByTestId("pairing-card")).not.toBeInTheDocument();
		await user.click(screen.getByTestId(`revoke-${PHONE_ID}`));
		expect(callsTo(harness.transport, "settings.iphone.revoke")).toEqual([
			{ method: "settings.iphone.revoke", params: { deviceID: PHONE_ID } },
		]);
		await user.click(screen.getByTestId("begin-pairing"));
		expect(
			callsTo(harness.transport, "settings.iphone.beginPairing"),
		).toHaveLength(1);
	});

	it("shows the pairing code with its expiry and a transfer arriving", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=pairing");
		renderWithBridge(<PhoneSection />, harness);
		const code = screen.getByRole("img", {
			name: "Pairing code for the Steno iPhone app",
		});
		expect(code).toHaveAttribute(
			"src",
			expect.stringMatching(/^data:image\/png;base64,/),
		);
		expect(screen.getByTestId("pairing-expiry")).toHaveTextContent(
			"It expires in 4 minutes.",
		);
		expect(screen.getByTestId("begin-pairing")).toBeDisabled();
		expect(
			screen.getByText("Receiving from Nicolai's iPhone…"),
		).toBeInTheDocument();
		expect(screen.getByRole("progressbar")).toHaveAttribute(
			"aria-valuenow",
			"50",
		);
		await user.click(screen.getByTestId("cancel-pairing"));
		expect(
			callsTo(harness.transport, "settings.iphone.cancelPairing"),
		).toHaveLength(1);
	});

	it("says when pairing is unavailable", async () => {
		const harness = await createBridgeHarness("scenario=phone-unavailable");
		renderWithBridge(<PhoneSection />, harness);
		expect(screen.getByTestId("phone-unavailable")).toHaveTextContent(
			"Pairing is unavailable right now.",
		);
		expect(screen.queryByTestId("begin-pairing")).not.toBeInTheDocument();
	});

	it("waits with Try again and Pair again instead of a new pairing", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=phone-waiting");
		renderWithBridge(<PhoneSection />, harness);
		const waiting = screen.getByTestId("phone-waiting");
		expect(waiting).toHaveTextContent(
			"Your phones can't send recordings right now.",
		);
		expect(screen.queryByTestId(`revoke-${PHONE_ID}`)).not.toBeInTheDocument();
		expect(screen.queryByTestId("begin-pairing")).not.toBeInTheDocument();
		await user.click(screen.getByTestId("try-again"));
		expect(callsTo(harness.transport, "settings.iphone.tryAgain")).toHaveLength(
			1,
		);
		await user.click(screen.getByTestId("pair-again"));
		expect(
			callsTo(harness.transport, "settings.iphone.pairAgain"),
		).toHaveLength(1);
	});

	it("turns both off while Try again or Pair again runs", async () => {
		const waiting = (await loadFixtureSnapshots())[
			"settings.iphone.waiting"
		] as PhoneSettingsSnapshot;
		const harness = await createBridgeHarness("", {
			"settings.iphone": {
				...waiting,
				listener: { state: "failed", failure: "The keychain is locked." },
				waiting: {
					reason: "unreadable",
					message: "Steno could not read this Mac's phone pairing.",
					busy: true,
				},
			} satisfies PhoneSettingsSnapshot,
		});
		renderWithBridge(<PhoneSection />, harness);
		expect(screen.getByTestId("try-again")).toBeDisabled();
		expect(screen.getByTestId("try-again")).toHaveTextContent("Try again");
		expect(screen.getByTestId("pair-again")).toBeDisabled();
		expect(screen.queryByTestId("listener-failed")).not.toBeInTheDocument();
	});

	it.each([
		"importDenied",
		"unreadable",
		"missing",
		"replaced",
		"partlyReplaced",
		"fileNotOpened",
		"clockBehind",
	] as const)(
		"offers Try again and Pair again while it waits: %s",
		async (reason) => {
			const waiting = (await loadFixtureSnapshots())[
				"settings.iphone.waiting"
			] as PhoneSettingsSnapshot;
			const harness = await createBridgeHarness("", {
				"settings.iphone": {
					...waiting,
					listener: { state: "failed", failure: "The keychain is locked." },
					waiting: { reason, message: "Steno waits.", busy: false },
				} satisfies PhoneSettingsSnapshot,
			});
			renderWithBridge(<PhoneSection />, harness);
			expect(screen.getByTestId("try-again")).toBeEnabled();
			expect(screen.getByTestId("pair-again")).toBeEnabled();
			// Pair again leads only when the pairing is gone or some phones
			// predate it.
			const startOver = reason === "missing" || reason === "partlyReplaced";
			const primary = startOver ? "pair-again" : "try-again";
			const other = startOver ? "try-again" : "pair-again";
			expect(screen.getByTestId(primary)).toHaveClass("bg-primary");
			expect(screen.getByTestId(other)).not.toHaveClass("bg-primary");
			expect(screen.queryByTestId("begin-pairing")).not.toBeInTheDocument();
			expect(
				screen.queryByTestId(`revoke-${PHONE_ID}`),
			).not.toBeInTheDocument();
			expect(screen.queryByTestId("listener-failed")).not.toBeInTheDocument();
		},
	);

	it("shows the listener's failure with its details", async () => {
		const user = userEvent.setup();
		const phone = (await loadFixtureSnapshots())[
			"settings.iphone"
		] as PhoneSettingsSnapshot;
		const harness = await createBridgeHarness("", {
			"settings.iphone": {
				...phone,
				listener: { state: "failed", failure: "Address already in use." },
			} satisfies PhoneSettingsSnapshot,
		});
		renderWithBridge(<PhoneSection />, harness);
		expect(screen.getByTestId("listener-failed")).toHaveTextContent(
			"Steno cannot receive recordings right now.",
		);
		await user.click(screen.getByText("Details"));
		expect(screen.getByText("Address already in use.")).toBeInTheDocument();
	});

	it("says where the Mac's local network switch is, on the Mac only", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		const { unmount } = renderWithBridge(<PhoneSection />, harness);
		await user.click(screen.getByTestId("phone-local-network-open"));
		expect(callsTo(harness.transport, "system.openSystemSettings")).toEqual([
			{
				method: "system.openSystemSettings",
				params: { kind: "localNetwork" },
			},
		]);
		unmount();
		for (const os of ["windows", "linux"] as const) {
			const { unmount: done } = renderWithBridge(
				<PhoneSection />,
				await createBridgeHarness(),
				platformFor(os),
			);
			expect(
				screen.queryByTestId("phone-local-network"),
			).not.toBeInTheDocument();
			done();
		}
	});
});
