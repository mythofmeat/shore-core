interface TimeGapProps {
  label: string;
}

export function TimeGap({ label }: TimeGapProps) {
  return <div className="time-gap">{label}</div>;
}
