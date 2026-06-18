export function Badge({ label }) {
  return <span>{label}</span>;
}

export const Root = () => (
  <section>
    <Badge label="semantic" />
  </section>
);
