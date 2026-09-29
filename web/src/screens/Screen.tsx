type Props = { title: string; note: string };

/** Placeholder for each screen until its milestone lands. */
export function Screen({ title, note }: Props) {
  return (
    <main>
      <header>
        <div className="eyebrow">Sourcer</div>
        <h1>{title}</h1>
      </header>
      <section className="panel">
        <p>{note}</p>
      </section>
    </main>
  );
}
