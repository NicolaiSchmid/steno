/** The message of a thrown value, whatever its type. */
export function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
