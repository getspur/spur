import { useEffect, useState } from "react";

export default function PresentChrome({
  current,
  total,
}: {
  current: number;
  total: number;
}) {
  const [showHints, setShowHints] = useState(true);

  useEffect(() => {
    const timeout = setTimeout(() => setShowHints(false), 2000);
    return () => clearTimeout(timeout);
  }, []);

  const displayCurrent = total > 0 ? current + 1 : 0;

  return (
    <>
      <div className="pointer-events-none fixed bottom-4 right-4 rounded bg-black/40 px-3 py-1 font-mono text-sm text-slate-300">
        {displayCurrent} / {total}
      </div>
      {showHints && (
        <div className="pointer-events-none fixed bottom-4 left-4 font-mono text-xs text-slate-500">
          {"->"} next · {"<-"} prev · S notes · B blackout · Esc exit
        </div>
      )}
    </>
  );
}
