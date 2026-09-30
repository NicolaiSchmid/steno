import { describe, expect, it, vi } from "vitest";
import { ContractViolation, createBridgeClient } from "./client";
import {
	createMockTransport,
	loadFixtureReplies,
	loadFixtureSnapshots,
} from "./mock-transport";

describe("bridge client", () => {
	it("delivers fixture snapshots parsed by topic", async () => {
		const transport = createMockTransport({ snapshots: loadFixtureSnapshots });
		const client = createBridgeClient(transport);
		const snapshots: string[] = [];
		client.subscribe("meetings.list", (list) => {
			snapshots.push(list.groups[0]?.meetings[0]?.title ?? "");
		});
		await transport.ready();
		expect(snapshots).toEqual(["Produktstrategie 90/10"]);
	});

	it("drops a snapshot that violates the contract and reports it", async () => {
		const transport = createMockTransport();
		const onInvalidSnapshot = vi.fn();
		const client = createBridgeClient(transport, { onInvalidSnapshot });
		const handler = vi.fn();
		client.subscribe("recording", handler);
		transport.emit("recording", { state: "flying" });
		expect(handler).not.toHaveBeenCalled();
		expect(onInvalidSnapshot).toHaveBeenCalledWith(
			"recording",
			expect.anything(),
		);
	});

	it("rejects params that violate the contract before calling the host", async () => {
		const transport = createMockTransport();
		const client = createBridgeClient(transport);
		await expect(
			// @ts-expect-error deliberately wrong shape
			client.call("meetings.setFilter", { filter: "everything" }),
		).rejects.toBeInstanceOf(ContractViolation);
		expect(transport.calls).toHaveLength(0);
	});

	it("parses recorded replies", async () => {
		const replies = await loadFixtureReplies();
		const transport = createMockTransport({ replies });
		const client = createBridgeClient(transport);
		const reply = await client.call("speakers.options", {
			speakerID: "00000000-0000-0000-0000-000000000023",
			query: "an",
		});
		expect(reply.options.map((option) => option.kind)).toEqual([
			"person",
			"person",
			"create",
			"unknown",
		]);
	});

	it("resolves to undefined for methods without a reply", async () => {
		const transport = createMockTransport();
		const client = createBridgeClient(transport);
		await expect(client.call("recording.stop")).resolves.toBeUndefined();
		expect(transport.calls).toEqual([
			{ method: "recording.stop", params: null },
		]);
	});
});
