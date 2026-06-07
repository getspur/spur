export type CellCapture = {
  webm_base64: string;
  duration_sec: number;
};

const captures = new Map<string, CellCapture>();

export function setCellCapture(cellId: string, capture: CellCapture) {
  captures.set(cellId, capture);
}

export function getCellCapture(cellId: string): CellCapture | undefined {
  return captures.get(cellId);
}

export function clearCellCaptures() {
  captures.clear();
}
