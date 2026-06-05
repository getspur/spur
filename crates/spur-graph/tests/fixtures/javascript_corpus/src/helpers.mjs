export function normalizeName(name) {
  return name.trim().toUpperCase();
}

export class BaseWidget {
  mount() {
    return normalizeName("mounted");
  }
}
