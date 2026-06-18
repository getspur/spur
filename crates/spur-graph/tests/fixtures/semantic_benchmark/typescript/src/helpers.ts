export class BaseView {
  mount(): void {}
}

export interface Renderable {
  render(): string;
}

export enum Mode {
  Fast = "fast",
}

export type ViewResult = {
  mode: Mode;
};

export function renderThing(): string {
  return "ready";
}

export function renderItem(value: string): string {
  return value.toUpperCase();
}

export function inlineRender(value: string): string {
  return value.trim();
}
