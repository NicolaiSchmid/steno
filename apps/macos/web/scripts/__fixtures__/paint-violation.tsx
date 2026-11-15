/**
 * Fixture for scripts/check-ui-restyle.test.ts: paint the contrast test
 * cannot see (a veiled token, a faded element) and the exceptions.
 */
export function PaintFixture() {
	return (
		<div className="bg-foreground/40 text-muted-foreground/80">
			<span className="opacity-50">Faded</span>
			<span className="w-1/2 disabled:opacity-64 data-starting-style:opacity-0">
				Exempt
			</span>
			<span aria-hidden="true">
				<i className="opacity-70" />
			</span>
		</div>
	);
}
