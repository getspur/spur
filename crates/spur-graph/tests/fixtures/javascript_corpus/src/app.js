import { Badge } from "./component.jsx";
import { BaseWidget, normalizeName } from "./helpers.mjs";

class Dashboard extends BaseWidget {
  render(name) {
    const label = normalizeName(name);
    Badge({ label });
    return label;
  }
}

export function createDashboard(name) {
  const dashboard = new Dashboard();
  dashboard.render(name);
  return dashboard;
}

createDashboard("demo");
