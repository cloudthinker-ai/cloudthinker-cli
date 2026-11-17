// Shared by the header and startup resources in the same terminal render pass.
let heroVisible = false;
let diagnosticRows = 0;

export function setStartupHeroVisible(visible: boolean): void {
	heroVisible = visible;
}

export function startupHeroVisible(): boolean {
	return heroVisible;
}

export function setStartupDiagnosticRows(rows: number): void {
	diagnosticRows = rows;
}

export function startupDiagnosticRows(): number {
	return diagnosticRows;
}

let detailRowsReader: (width: number) => number = () => 0;
let detailsRevision = 0;

export function setStartupDetailRowsReader(reader: (width: number) => number): void {
	detailRowsReader = reader;
	detailsRevision += 1;
}

export function startupDetailRows(width: number): number {
	return detailRowsReader(width);
}

export function startupDetailsRevision(): number {
	return detailsRevision;
}
