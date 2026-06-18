export class BaseWidget {
  mount() {}
}

export function normalizeName(value) {
  return String(value).trim();
}

export function inlineName(value) {
  return value.toLowerCase();
}
