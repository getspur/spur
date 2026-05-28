type Props = { notes?: string };

export default function SpeakerNotes({ notes }: Props) {
  if (!notes) return null;

  return (
    <div className="fixed inset-x-[8%] bottom-[6%] rounded-lg border-l-[3px] border-blue-500 bg-slate-900/95 p-5 font-mono text-sm leading-relaxed text-slate-50 shadow-2xl">
      <div className="mb-2 text-[10px] uppercase tracking-widest text-slate-400">
        speaker notes · press S to toggle
      </div>
      {notes}
    </div>
  );
}
