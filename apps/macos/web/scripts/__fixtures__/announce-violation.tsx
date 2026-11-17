/**
 * Fixture for scripts/check-ui-restyle.test.ts: live regions declared
 * outside the kit.
 */
export function AnnounceFixture() {
	return (
		<div>
			<p role="status">Saved.</p>
			<p aria-live="polite">Exported.</p>
			<p role="note">Not a live region.</p>
		</div>
	);
}
