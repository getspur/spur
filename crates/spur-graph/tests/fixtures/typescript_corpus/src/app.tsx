import { Helper, Mode, renderThing } from "./helpers";

interface Props {
  mode: Mode;
}

type AppResult = Props;

class App extends Helper {
  boot() {
    renderThing();
    this.run();
  }
}

function createApp(mode: Mode): AppResult {
  const app = new App();
  app.boot();
  return { mode };
}

const helper = () => 42;

export const Greeting = ({ name }: { name: string }) => <div>{name}</div>;
