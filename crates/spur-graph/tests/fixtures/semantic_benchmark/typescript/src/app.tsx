import {
  BaseView,
  Mode,
  Renderable,
  ViewResult,
  inlineRender,
  renderItem,
  renderThing,
} from "./helpers";

type Props = {
  name: string;
};

export class Dashboard extends BaseView implements Renderable {
  boot(): void {
    renderThing();
    this.mount();
  }

  render(): string {
    return renderThing();
  }
}

export function createDashboard(mode: Mode): ViewResult {
  const dashboard = new Dashboard();
  dashboard.boot();
  return { mode };
}

export const Greeting = ({ name }: Props) => <span>{name}</span>;

export const Root = () => (
  <div>
    <Greeting name="semantic" />
  </div>
);

export function renderList(items: string[]): string[] {
  return items.map(renderItem).map((value) => inlineRender(value));
}
