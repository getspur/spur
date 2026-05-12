export interface Runner {
  run(): void;
}

export type Result = "ok" | "fail";

export enum Mode {
  Fast,
  Slow,
}

export function renderThing(): Result {
  return "ok";
}

export class Helper implements Runner {
  run(): void {
    renderThing();
  }
}
