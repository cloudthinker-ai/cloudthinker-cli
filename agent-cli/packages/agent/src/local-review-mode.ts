export const LOCAL_REVIEW_FLAG = "--cloudthinker-local-review";

export function isLocalReview(args: string[]): boolean {
	return args.includes(LOCAL_REVIEW_FLAG);
}

export function withoutLocalReviewFlag(args: string[]): string[] {
	return args.filter((arg) => arg !== LOCAL_REVIEW_FLAG);
}
